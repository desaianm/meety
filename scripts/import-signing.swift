import Foundation
import Security

func checked(_ status: OSStatus) {
    guard status == errSecSuccess else {
        let message = SecCopyErrorMessageString(status, nil) as String? ?? "status \(status)"
        fputs("Keychain import failed: \(message)\n", stderr)
        exit(1)
    }
}
guard CommandLine.arguments.count == 3, let password = readLine() else { exit(2) }
var keychain: SecKeychain?
checked(SecKeychainOpen(CommandLine.arguments[2], &keychain))
var trusted: SecTrustedApplication?
checked(SecTrustedApplicationCreateFromPath("/usr/bin/codesign", &trusted))
var access: SecAccess?
checked(SecAccessCreate("Meetlocal Local Signing" as CFString, [trusted!] as CFArray, &access))
var parameters = SecItemImportExportKeyParameters()
parameters.version = UInt32(SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION)
let passphrase = password as CFString
let usage = [kSecAttrCanSign] as CFArray
let attributes = [kSecAttrIsPermanent, kSecAttrIsSensitive] as CFArray
parameters.passphrase = .passUnretained(passphrase)
parameters.accessRef = .passUnretained(access!)
parameters.keyUsage = .passUnretained(usage)
parameters.keyAttributes = .passUnretained(attributes)
var format = SecExternalFormat.formatPKCS12
var type = SecExternalItemType.itemTypeAggregate
var items: CFArray?
let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
withExtendedLifetime((passphrase, access, usage, attributes)) {
    checked(SecItemImport(data as CFData, nil, &format, &type, [], &parameters, keychain, &items))
}
