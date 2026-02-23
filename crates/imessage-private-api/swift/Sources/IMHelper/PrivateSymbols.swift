import Foundation

// MARK: - Private Framework Symbol Resolution

/// IDS functions (availability checks)
typealias IDSCopyIDFunc = @convention(c) (CFString) -> Unmanaged<AnyObject>

/// IMCore functions
typealias IMCreateThreadIDFunc = @convention(c) (AnyObject) -> Unmanaged<NSString>

/// Runtime-resolved function pointers (set by resolvePrivateSymbols).
var resolved_IDSCopyIDForPhoneNumber: IDSCopyIDFunc?
var resolved_IDSCopyIDForEmailAddress: IDSCopyIDFunc?
var resolved_IDSServiceNameiMessage: String?
var resolved_IDSServiceNameFaceTime: String?
var resolved_IMCreateThreadIdentifier: IMCreateThreadIDFunc?

/// RTLD_DEFAULT on macOS is ((void*)-2)
private let RTLD_DEFAULT_PTR = UnsafeMutableRawPointer(bitPattern: -2)

/// Resolve IDS and IMCore private symbols at runtime.
/// Must be called once during bootstrap.
func resolvePrivateSymbols() {
    // IDS functions
    if let ptr = dlsym(RTLD_DEFAULT_PTR, "IDSCopyIDForPhoneNumber") {
        resolved_IDSCopyIDForPhoneNumber = unsafeBitCast(ptr, to: IDSCopyIDFunc.self)
    } else {
        Log.info("IDSCopyIDForPhoneNumber not found")
    }

    if let ptr = dlsym(RTLD_DEFAULT_PTR, "IDSCopyIDForEmailAddress") {
        resolved_IDSCopyIDForEmailAddress = unsafeBitCast(ptr, to: IDSCopyIDFunc.self)
    } else {
        Log.info("IDSCopyIDForEmailAddress not found")
    }

    // IDS service name constants — global NSString* variables.
    // dlsym returns the ADDRESS of the global variable (a pointer to the CFString pointer).
    if let ptr = dlsym(RTLD_DEFAULT_PTR, "IDSServiceNameiMessage") {
        let strPtr = ptr.assumingMemoryBound(to: UnsafeRawPointer?.self)
        if let rawPtr = strPtr.pointee {
            resolved_IDSServiceNameiMessage = Unmanaged<NSString>.fromOpaque(rawPtr).takeUnretainedValue() as String
        }
    } else {
        Log.info("IDSServiceNameiMessage not found")
    }

    if let ptr = dlsym(RTLD_DEFAULT_PTR, "IDSServiceNameFaceTime") {
        let strPtr = ptr.assumingMemoryBound(to: UnsafeRawPointer?.self)
        if let rawPtr = strPtr.pointee {
            resolved_IDSServiceNameFaceTime = Unmanaged<NSString>.fromOpaque(rawPtr).takeUnretainedValue() as String
        }
    } else {
        Log.info("IDSServiceNameFaceTime not found")
    }

    // IMCore functions
    if let ptr = dlsym(RTLD_DEFAULT_PTR, "IMCreateThreadIdentifierForMessagePartChatItem") {
        resolved_IMCreateThreadIdentifier = unsafeBitCast(ptr, to: IMCreateThreadIDFunc.self)
    } else {
        Log.info("IMCreateThreadIdentifierForMessagePartChatItem not found")
    }

    Log.info("private symbols resolved")
}
