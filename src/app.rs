use crate::{audio::Capture, model, store::Paths, transcribe};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    process::Command,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS},
    window::WindowBuilder,
};
use tray_icon::{
    menu::{
        accelerator::{Accelerator, Code, Modifiers},
        Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    },
    Icon, TrayIconBuilder,
};

#[derive(Clone, Default)]
struct State {
    menu_bar_ready: bool,
    active: Option<String>,
    preparing: bool,
    elapsed: u64,
    transcribing: bool,
    metrics: Value,
    error: Option<String>,
}
impl State {
    fn json(&self) -> Value {
        json!({"app_running":true,"menu_bar_ready":self.menu_bar_ready,"preparing":self.preparing,"microphone_permission":hypr_capture::microphone_permission(),"recording":self.active.is_some(),"meeting_id":self.active,"elapsed_seconds":self.elapsed,"transcribing":self.transcribing,"audio":self.metrics,"error":self.error})
    }
}
struct Request {
    value: Value,
    reply: mpsc::Sender<Result<Value>>,
}
enum UiEvent {
    Menu(MenuEvent),
    Message(Value),
    Response(Value),
}

pub fn request(p: &Paths, value: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(p.socket())
        .context("Meetlocal menu bar app is not running. Open Meetlocal.app first")?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut stream, &value)?;
    writeln!(&mut stream)?;
    let mut line = String::new();
    BufReader::new(stream).take(65537).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response["error"].as_str() {
        bail!("{error}");
    }
    Ok(response["result"].clone())
}
fn serve_connection(mut stream: UnixStream, tx: &mpsc::Sender<Request>, state: &Arc<Mutex<State>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut line = String::new();
    if BufReader::new(copy)
        .take(65537)
        .read_line(&mut line)
        .is_err()
        || line.len() > 65536
    {
        return;
    }
    let result = (|| -> Result<Value> {
        let value: Value = serde_json::from_str(&line)?;
        if value["op"] == "status" {
            return Ok(state.lock().unwrap().json());
        }
        if !matches!(value["op"].as_str(), Some("start" | "stop" | "delete")) {
            bail!("Unknown app command");
        }
        if state.lock().unwrap().preparing {
            bail!("Audio startup is pending; wait for permissions or its timeout");
        }
        let (reply, wait) = mpsc::channel();
        tx.send(Request { value, reply })?;
        wait.recv_timeout(Duration::from_secs(60))
            .context("App command timed out")?
    })();
    let response = match result {
        Ok(v) => json!({"result":v}),
        Err(e) => json!({"error":format!("{e:#}")}),
    };
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = writeln!(&mut stream);
}
pub fn run(p: Paths) -> Result<()> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p.root.join("app.lock"))?;
    lock.try_lock_exclusive()
        .context("Meetlocal is already running")?;
    p.recover()?;
    let socket = p.socket();
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let state = Arc::new(Mutex::new(State::default()));
    let (tx, rx) = mpsc::channel::<Request>();
    let worker_paths = p.clone();
    let worker_state = state.clone();
    std::thread::spawn(move || worker(worker_paths, worker_state, rx));
    let server_tx = tx.clone();
    let server_state = state.clone();
    std::thread::spawn(move || {
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else {
                continue;
            };
            use std::sync::atomic::Ordering;
            if connections.fetch_add(1, Ordering::Relaxed) >= 16 {
                connections.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            let tx = server_tx.clone();
            let state = server_state.clone();
            let count = connections.clone();
            std::thread::spawn(move || {
                serve_connection(stream, &tx, &state);
                count.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Regular);
    let window = WindowBuilder::new()
        .with_title("Meetlocal")
        .with_inner_size(tao::dpi::LogicalSize::new(1050.0, 740.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(780.0, 500.0))
        .build(&event_loop)?;
    let ipc = event_loop.create_proxy();
    let webview = wry::WebViewBuilder::new()
        .with_html(include_str!("../ui/index.html"))
        .with_navigation_handler(|url| url == "about:blank")
        .with_ipc_handler(move |request| {
            if request.body().len() > 1_100_000 {
                return;
            }
            if let Ok(value) = serde_json::from_str(request.body()) {
                let _ = ipc.send_event(UiEvent::Message(value));
            }
        })
        .build(&window)?;
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UiEvent::Menu(event));
    }));
    let menu = Menu::new();
    let open_app = MenuItem::new("Open Meetlocal", true, None);
    let status = MenuItem::new("Meetlocal · Ready", false, None);
    let toggle = MenuItem::new("Start Recording", true, None);
    let latest = MenuItem::new("Open Latest Transcript", true, None);
    let archive = MenuItem::new("Open Local Archive", true, None);
    let permissions = MenuItem::new("Audio Permissions…", true, None);
    let download = MenuItem::new("Download English Model", true, None);
    let small = MenuItem::new("Use Small Model · Less Memory", true, None);
    let medium = MenuItem::new("Use Medium Model · Better Accuracy", true, None);
    let guide = MenuItem::new("CLI & Agent Setup…", true, None);
    let quit = MenuItem::new(
        "Quit Meetlocal",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
    );
    let application_menu = Menu::new();
    let app_menu = Submenu::new("Meetlocal", true);
    app_menu.append_items(&[&open_app, &PredefinedMenuItem::separator(), &quit])?;
    let edit_menu = Submenu::new("Edit", true);
    edit_menu.append_items(&[
        &PredefinedMenuItem::undo(None),
        &PredefinedMenuItem::redo(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::select_all(None),
    ])?;
    application_menu.append_items(&[&app_menu, &edit_menu])?;
    application_menu.init_for_nsapp();
    menu.append_items(&[
        &open_app,
        &status,
        &PredefinedMenuItem::separator(),
        &toggle,
        &latest,
        &archive,
        &PredefinedMenuItem::separator(),
        &download,
        &small,
        &medium,
        &permissions,
        &guide,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
    let mut tray = None;
    let mut downloading = false;
    let (download_done, download_results) = mpsc::channel();
    let mut last_ui_update = Instant::now() - Duration::from_secs(2);
    let ui_proxy = event_loop.create_proxy();
    event_loop.run(move|event,target,control_flow|{
        // Keep the process lock alive until the app really exits.
        let _keep_lock=&lock;
        let _keep_native_menu=&application_menu;
        *control_flow=ControlFlow::WaitUntil(Instant::now()+Duration::from_secs(1));
        match event {
            Event::NewEvents(StartCause::Init)=>{
                tray=Some(TrayIconBuilder::new().with_menu(Box::new(menu.clone())).with_tooltip("Meetlocal · Offline meeting transcripts").with_icon_templated(icon(false)).build().expect("Create macOS menu bar icon"));
                state.lock().unwrap().menu_bar_ready=true;
            },
            Event::UserEvent(UiEvent::Menu(event))=>{
                let snapshot=state.lock().unwrap().clone();
                if event.id==open_app.id(){target.set_activation_policy_at_runtime(ActivationPolicy::Regular);window.set_visible(true);window.set_focus();}
                else if event.id==toggle.id(){
                    if !snapshot.preparing && !snapshot.transcribing && !downloading {
                        let value=if snapshot.active.is_some(){json!({"op":"stop"})}else{json!({"op":"start"})};
                        let (reply,_)=mpsc::channel();let _=tx.send(Request{value,reply});
                    }
                }else if event.id==latest.id(){
                    if let Ok(t)=p.get("latest"){
                        let path=p.meeting_dir(&t.meeting.id).join("transcript.md");
                        if path.exists(){let _=Command::new("/usr/bin/open").arg(path).spawn();}
                        else{state.lock().unwrap().error=Some("Latest meeting has no transcript yet".into());}
                    }
                }else if event.id==archive.id(){let _=Command::new("/usr/bin/open").arg(p.root.join("meetings")).spawn();}
                else if event.id==permissions.id(){let _=Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture").spawn();}
                else if event.id==guide.id(){
                    let executable=std::env::current_exe().unwrap_or_default();
                    let file=p.root.join("Agent Setup.txt");
                    let text=format!("Meetlocal — local English meeting transcripts\n\nOpen this app, then click Start Recording. Grant Microphone and Screen & System Audio Recording permission in macOS. Use headphones to avoid microphone echo. Stop Recording transcribes locally.\n\nCLI:\nmeetlocal list\nmeetlocal get latest\nmeetlocal search \"roadmap\"\nmeetlocal agent-setup\n\nRead-only MCP executable:\n{} mcp\n\nAudio and transcripts remain on this Mac. Asking a cloud agent to read a transcript sends the returned text to that agent's provider.\n\nStorage: {}\n",executable.display(),p.root.display());
                    let _=crate::store::atomic_write(&file,text.as_bytes());let _=Command::new("/usr/bin/open").arg(file).spawn();
                }else if event.id==small.id()||event.id==medium.id(){
                    let choice=if event.id==small.id(){model::Choice::Small}else{model::Choice::Medium};
                    if snapshot.active.is_none()&&!snapshot.transcribing&&!downloading {
                        if model::ready(&p,choice){if let Err(e)=model::select(&p,choice){state.lock().unwrap().error=Some(e.to_string());}}
                        else{state.lock().unwrap().error=Some(format!("Install first: meetlocal model install {}",choice.name()));}
                    }
                }else if event.id==download.id()&&!downloading&&!snapshot.transcribing&&snapshot.active.is_none(){
                    downloading=true;let p=p.clone();let done=download_done.clone();
                    std::thread::spawn(move||{let r=model::install(&p,model::selected(&p)).map_err(|e|format!("{e:#}"));let _=done.send(r);});
                }else if event.id==quit.id(){
                    if snapshot.active.is_some(){state.lock().unwrap().error=Some("Stop Recording before quitting".into());}else if snapshot.preparing{state.lock().unwrap().error=Some("Wait for audio startup or its timeout before quitting".into());}
                    else if snapshot.transcribing||downloading{state.lock().unwrap().error=Some("Wait for transcription or download before quitting".into());}
                    else{let _=fs::remove_file(&socket);*control_flow=ControlFlow::Exit;}
                }
            },
            Event::UserEvent(UiEvent::Message(message))=>{
                let p=p.clone();let state=state.clone();let tx=tx.clone();let proxy=ui_proxy.clone();
                std::thread::spawn(move||{let rid=message["rid"].clone();let result=ui_request(&p,&state,&tx,&message);let response=match result{Ok(value)=>json!({"rid":rid,"result":value}),Err(e)=>json!({"rid":rid,"error":format!("{e:#}")})};let _=proxy.send_event(UiEvent::Response(response));});
            },
            Event::UserEvent(UiEvent::Response(response))=>{let _=webview.evaluate_script(&format!("window.meetlocalResponse({response})"));},
            Event::WindowEvent{event:WindowEvent::CloseRequested,..}=>{window.set_visible(false);target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);},
            _=>{}
        }
        if last_ui_update.elapsed()<Duration::from_millis(500){return;}
        last_ui_update=Instant::now();
        if let Ok(result)=download_results.try_recv(){downloading=false;state.lock().unwrap().error=result.err();}
        let snapshot=state.lock().unwrap().clone();let choice=model::selected(&p);
        let text=if downloading{"Downloading English model…".into()}else if snapshot.preparing{"Starting audio… Check macOS permissions".into()}else if snapshot.active.is_some(){format!("Recording · {:02}:{:02}",snapshot.elapsed/60,snapshot.elapsed%60)}else if snapshot.transcribing{"Transcribing locally…".into()}else if let Some(error)=&snapshot.error{format!("Error · {}",error.chars().take(100).collect::<String>())}else{format!("Ready · English · {}",choice.name())};
        status.set_text(&text);toggle.set_text(if snapshot.active.is_some(){"Stop & Transcribe"}else{"Start Recording"});toggle.set_enabled(!snapshot.preparing&&!snapshot.transcribing&&!downloading);
        download.set_enabled(!downloading&&!snapshot.transcribing&&snapshot.active.is_none());
        if let Some(tray)=&tray{
            let _=tray.set_tooltip(Some(&text));
            if snapshot.active.is_some(){let _=tray.set_icon(Some(icon(true)));}else{let _=tray.set_icon_templated(Some(icon(false)));}
            tray.set_title(if snapshot.active.is_some(){Some(format!("{:02}:{:02}",snapshot.elapsed/60,snapshot.elapsed%60))}else{None});
        }
    });
}
fn ui_request(
    p: &Paths,
    state: &Arc<Mutex<State>>,
    tx: &mpsc::Sender<Request>,
    message: &Value,
) -> Result<Value> {
    match message["op"].as_str().unwrap_or("") {
        "list" => Ok(serde_json::to_value(p.list(500)?)?),
        "get" => Ok(serde_json::to_value(
            p.get(message["id"].as_str().context("Missing meeting id")?)?,
        )?),
        "new" => Ok(json!(p.create("Untitled meeting", "draft")?)),
        "save" => {
            p.save_notes(
                message["id"].as_str().context("Missing id")?,
                message["title"].as_str().context("Missing title")?,
                message["notes"].as_str().context("Missing notes")?,
            )?;
            Ok(json!({"saved":true}))
        }
        "status" => Ok(state.lock().unwrap().json()),
        "search" => Ok(serde_json::to_value(
            p.search(message["query"].as_str().unwrap_or(""), 100)?,
        )?),
        "model_info" => {
            Ok(json!({"choice":model::selected(p),"ready":model::ready(p,model::selected(p))}))
        }
        "model" => {
            let choice: model::Choice = serde_json::from_value(message["choice"].clone())?;
            if !model::ready(p, choice) {
                bail!("Download this model from the menu bar first");
            }
            model::select(p, choice)?;
            Ok(json!({"choice":choice}))
        }
        "permissions" => {
            Command::new("/usr/bin/open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
                .spawn()?;
            Ok(json!({}))
        }
        "open" => {
            let id = p.resolve(message["id"].as_str().context("Missing id")?)?;
            let file = p.meeting_dir(&id).join("transcript.md");
            if !file.exists() {
                bail!("Transcript not ready yet");
            }
            Command::new("/usr/bin/open").arg(file).spawn()?;
            Ok(json!({}))
        }
        "start" | "stop" | "delete" => {
            let (reply, wait) = mpsc::channel();
            tx.send(Request {
                value: message.clone(),
                reply,
            })?;
            wait.recv_timeout(Duration::from_secs(60))
                .context("Audio command timed out")?
        }
        _ => bail!("Unknown app action"),
    }
}
fn worker(p: Paths, state: Arc<Mutex<State>>, rx: mpsc::Receiver<Request>) {
    let mut capture: Option<(String, Capture)> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(request) => {
                let result = (|| -> Result<Value> {
                    match request.value["op"].as_str().unwrap_or("") {
                        "start" => {
                            if capture.is_some() {
                                bail!("Already recording");
                            }
                            if state.lock().unwrap().transcribing {
                                bail!("Wait for transcription to finish");
                            }
                            let choice = model::selected(&p);
                            if !model::ready(&p, choice) {
                                bail!("Download English model before recording");
                            }
                            let title = request.value["title"]
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|| {
                                    format!(
                                        "Meeting {}",
                                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                                    )
                                });
                            let id = if let Some(id) = request.value["id"].as_str() {
                                let t = p.get(id)?;
                                if t.meeting.status != "draft" {
                                    bail!("Create a new meeting before recording");
                                }
                                p.set_status(&t.meeting.id, "recording", None)?;
                                t.meeting.id
                            } else {
                                p.create(&title, "recording")?
                            };
                            state.lock().unwrap().preparing = true;
                            let started = Capture::start(&p.meeting_dir(&id));
                            state.lock().unwrap().preparing = false;
                            match started {
                                Ok(audio) => {
                                    capture = Some((id.clone(), audio));
                                    let mut s = state.lock().unwrap();
                                    s.active = Some(id.clone());
                                    s.error = None;
                                    s.elapsed = 0;
                                    Ok(json!({"id":id,"recording":true}))
                                }
                                Err(e) => {
                                    p.set_status(&id, "failed", Some(&format!("{e:#}")))?;
                                    Err(e)
                                }
                            }
                        }
                        "delete" => {
                            if capture.is_some() || state.lock().unwrap().transcribing {
                                bail!("Wait for recording and transcription to finish before deleting a meeting");
                            }
                            let id = p.resolve(
                                request.value["id"].as_str().context("Missing meeting id")?,
                            )?;
                            p.delete(&id)?;
                            state.lock().unwrap().error = None;
                            Ok(json!({"deleted":true,"id":id}))
                        }
                        "stop" => {
                            let (id, audio) = capture.take().context("No active recording")?;
                            state.lock().unwrap().active = None;
                            if let Err(e) = audio.stop() {
                                p.set_status(&id, "failed", Some(&format!("{e:#}")))?;
                                return Err(e);
                            }
                            p.set_status(&id, "ready", None)?;
                            state.lock().unwrap().transcribing = true;
                            let paths = p.clone();
                            let status = state.clone();
                            let job_id = id.clone();
                            std::thread::spawn(move || {
                                let result = transcribe::run(&paths, &job_id);
                                let mut s = status.lock().unwrap();
                                s.transcribing = false;
                                s.error = result.err().map(|e| format!("{e:#}"));
                            });
                            Ok(json!({"id":id,"recording":false,"transcribing":true}))
                        }
                        _ => bail!("Unknown app command"),
                    }
                })();
                if let Err(e) = &result {
                    state.lock().unwrap().error = Some(format!("{e:#}"));
                }
                let _ = request.reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some((id, audio)) = &capture {
            let mut s = state.lock().unwrap();
            s.elapsed = audio.started.elapsed().as_secs();
            s.metrics = audio.metrics();
            if let Some(error) = audio.error() {
                s.error = Some(error.clone());
                let _ = p.set_status(id, "failed", Some(&error));
            }
        }
    }
}
fn icon(recording: bool) -> Icon {
    let mut rgba = vec![0; 24 * 24 * 4];
    for y in 0..24 {
        for x in 0..24 {
            let dx = x as f64 - 11.5;
            let dy = y as f64 - 11.5;
            let radius = (dx * dx + dy * dy).sqrt();
            let ring = (8.3..=9.7).contains(&radius);
            let wave = (x == 7 || x == 8) && (9..=14).contains(&y)
                || (x == 11 || x == 12) && (6..=17).contains(&y)
                || (x == 15 || x == 16) && (9..=14).contains(&y);
            if ring || wave {
                let i = (y * 24 + x) * 4;
                rgba[i..i + 4].copy_from_slice(if recording {
                    &[235, 66, 66, 255]
                } else {
                    &[0, 0, 0, 255]
                });
            }
        }
    }
    Icon::from_rgba(rgba, 24, 24).unwrap()
}
