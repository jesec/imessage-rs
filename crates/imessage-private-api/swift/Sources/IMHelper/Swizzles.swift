import Foundation
import ObjectiveC

// MARK: - Swizzle Infrastructure

/// Saved original IMPs for each hook.
private var origHandleIncomingItem: IMP?
private var origDidReceiveLocation: IMP?
private var origRegistrationStatusChanged: IMP?
private var origSetShowTypingIndicator: IMP?

/// Install a single method swizzle.
private func installSwizzle(_ className: String, _ selectorName: String, _ replacement: IMP, _ outOriginal: inout IMP?) {
    guard let cls = NSClassFromString(className) else {
        Log.info("swizzle target class \(className) not found, skipping")
        return
    }
    let sel = NSSelectorFromString(selectorName)
    guard let method = class_getInstanceMethod(cls, sel) else {
        Log.info("swizzle target method \(className).\(selectorName) not found, skipping")
        return
    }
    outOriginal = method_getImplementation(method)
    method_setImplementation(method, replacement)
    Log.info("swizzled \(className).\(selectorName)")
}

// MARK: - Hook 1: IMChat._handleIncomingItem: (typing indicators, Sequoia)

private let swizzledHandleIncomingItem: @convention(c) (AnyObject, Selector, AnyObject) -> Bool = { _self, _cmd, arg1 in
    // Call original first
    let original = unsafeBitCast(origHandleIncomingItem!, to: (@convention(c) (AnyObject, Selector, AnyObject) -> Bool).self)
    let result = original(_self, _cmd, arg1)

    // Extract chat GUID via ivar or accessor
    var guid: String?
    if let cls = object_getClass(_self),
       let ivar = class_getInstanceVariable(cls, "_guid") {
        guid = object_getIvar(_self, ivar) as? String
    }
    if guid == nil, _self.responds(to: NSSelectorFromString("guid")) {
        guid = _self.perform(NSSelectorFromString("guid"))?.takeUnretainedValue() as? String
    }

    if let guid = guid {
        handleIncomingItem(arg1, guid as NSString)
    }

    return result
}

// MARK: - Hook 2: IMFMFSession.didReceiveLocationForHandle: (FindMy location streaming, Sequoia+)

private let swizzledDidReceiveLocation: @convention(c) (AnyObject, Selector, AnyObject) -> Void = { _self, _cmd, arg1 in
    // Call original first
    let original = unsafeBitCast(origDidReceiveLocation!, to: (@convention(c) (AnyObject, Selector, AnyObject) -> Void).self)
    original(_self, _cmd, arg1)

    // Forward to Swift handler
    handleLocationUpdateForHandle(arg1)
}

// MARK: - Hook 3: IMAccount._registrationStatusChanged: (alias monitoring)

private let swizzledRegistrationStatusChanged: @convention(c) (AnyObject, Selector, AnyObject) -> Void = { _self, _cmd, arg1 in
    // Call original first
    let original = unsafeBitCast(origRegistrationStatusChanged!, to: (@convention(c) (AnyObject, Selector, AnyObject) -> Void).self)
    original(_self, _cmd, arg1)
    // Forward to Swift
    if let notification = arg1 as? NSNotification {
        handleRegistrationStatusChanged(notification)
    }
}

// MARK: - Hook 4: CKConversationListStandardCell.setShowTypingIndicator: (Tahoe)

private let swizzledSetShowTypingIndicator: @convention(c) (AnyObject, Selector, Bool) -> Void = { _self, _cmd, show in
    // Call original first
    let original = unsafeBitCast(origSetShowTypingIndicator!, to: (@convention(c) (AnyObject, Selector, Bool) -> Void).self)
    original(_self, _cmd, show)

    // Only active on macOS 26+ (Tahoe)
    if ProcessInfo.processInfo.operatingSystemVersion.majorVersion < 26 {
        return
    }

    handleTahoeTypingIndicator(show, _self)
}

// MARK: - Install All Swizzles

/// Install all method swizzles (typing indicators, FindMy, alias monitoring, Tahoe typing).
/// Called from IMHelper.bootstrap() for Messages.app only.
func installSwizzles() {
    Log.info("installing swizzles...")

    // Hook 1: typing indicators (Sequoia path)
    installSwizzle("IMChat", "_handleIncomingItem:",
                    unsafeBitCast(swizzledHandleIncomingItem, to: IMP.self),
                    &origHandleIncomingItem)

    // Hook 2: FindMy locations (Sequoia+)
    // Replaces the old FMFSessionDataManager.setLocations: hook which doesn't fire on Sequoia+.
    // IMFMFSession.didReceiveLocationForHandle: is called by IMCore whenever the FindMy framework
    // delivers a location update. On Sequoia, arg is IMHandle; on Tahoe, arg is IMFindMyHandle.
    installSwizzle("IMFMFSession", "didReceiveLocationForHandle:",
                    unsafeBitCast(swizzledDidReceiveLocation, to: IMP.self),
                    &origDidReceiveLocation)

    // Hook 3: alias monitoring
    installSwizzle("IMAccount", "_registrationStatusChanged:",
                    unsafeBitCast(swizzledRegistrationStatusChanged, to: IMP.self),
                    &origRegistrationStatusChanged)

    // Hook 4: Tahoe typing indicators (macOS 26+)
    if ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 26 {
        installSwizzle("CKConversationListStandardCell", "setShowTypingIndicator:",
                        unsafeBitCast(swizzledSetShowTypingIndicator, to: IMP.self),
                        &origSetShowTypingIndicator)
    }

    Log.info("swizzles installed")
}
