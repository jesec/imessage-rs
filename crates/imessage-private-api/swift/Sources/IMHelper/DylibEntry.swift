import Foundation
import os.log

/// Dylib entry point, registered via the linker's `-init` flag.
/// Fires when DYLD_INSERT_LIBRARIES loads us into Messages.app, FaceTime.app, or FindMy.app.
@_cdecl("_dylib_init")
public func _dylibInit() {
    let bundleId = Bundle.main.bundleIdentifier ?? "unknown"
    Log.info("loaded into \(bundleId)")

    let isMessages = bundleId == "com.apple.MobileSMS" || bundleId == "com.apple.Messages"
    let isFaceTime = bundleId == "com.apple.FaceTime" || bundleId == "com.apple.TelephonyUtilities"
    let isFindMy = bundleId == "com.apple.findmy"

    guard isMessages || isFaceTime || isFindMy else {
        Log.info("not a target process, skipping")
        return
    }

    // Bootstrap immediately. The TCP server binds before injection starts,
    // and TCPClient retries on connection failure, so no delay is needed.
    DispatchQueue.main.async {
        Log.info("bootstrapping...")
        IMHelper.bootstrap()
    }
}
