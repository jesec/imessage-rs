import Foundation
import CoreLocation

// MARK: - Logging

import os.log

private let logger = OSLog(subsystem: "com.imessage-helper", category: "main")

enum Log {
    static func info(_ message: String) {
        os_log(.default, log: logger, "%{public}@", message)
    }

    static func error(_ message: String) {
        os_log(.error, log: logger, "ERROR: %{public}@", message)
    }
}

// MARK: - Operating Mode

enum HelperMode {
    case messages
    case faceTime
    case findMy
}

// MARK: - IMHelper

/// Main entry point for the helper dylib.
/// Called from DylibEntry.swift via the linker -init hook.
class IMHelper {
    static var shared: IMHelper?
    static var tcp: TCPClient?
    static var mode: HelperMode = .messages

    static func bootstrap() {
        let bundleId = Bundle.main.bundleIdentifier ?? "unknown"
        Log.info("bootstrap: bundleId = \(bundleId)")

        let isMessages = bundleId == "com.apple.MobileSMS" || bundleId == "com.apple.Messages"
        let isFaceTime = bundleId == "com.apple.FaceTime" || bundleId == "com.apple.TelephonyUtilities"
        let isFindMy = bundleId == "com.apple.findmy"

        guard isMessages || isFaceTime || isFindMy else {
            Log.error("bootstrap: unsupported process \(bundleId)")
            return
        }

        if isMessages { mode = .messages }
        else if isFaceTime { mode = .faceTime }
        else { mode = .findMy }

        shared = IMHelper()

        // Resolve private framework symbols (IDS, IMCore) — not needed for FindMy
        if !isFindMy {
            resolvePrivateSymbols()
        }

        let client = TCPClient()
        tcp = client

        client.onConnect = {
            Log.info("connected, sending ping")
            client.send([
                "event": "ping",
                "message": "Helper Connected!",
                "process": bundleId,
            ])

            if isFindMy {
                // FindMy needs no framework init — ready immediately
                client.send(["event": "ready", "process": bundleId])
            }

            if isFaceTime {
                FaceTimeActions.registerCallObservers()
                // FaceTime uses CallKit, not IMCore — ready immediately
                client.send(["event": "ready", "process": bundleId])
            }

            if isMessages {
                // Eagerly initialize IMCore singletons so the first PA action
                // doesn't hang for 30-120s. Runs on the main queue (required by
                // IMCore APIs). Sends "ready" when done.
                DispatchQueue.main.async {
                    Log.info("eagerly initializing IMCore...")
                    if let controller = getSharedInstance("IMAccountController") {
                        // Touch activeIMessageAccount to force the full imagent
                        // daemon connection and IDS subsystem initialization.
                        let _ = safePerformReturning(controller, selector: "activeIMessageAccount")
                    }
                    let _ = getSharedInstance("IMChatRegistry")
                    Log.info("IMCore initialized, sending ready")
                    client.send(["event": "ready", "process": bundleId])
                }
            }
        }

        client.onMessage = { message in
            IMHelper.handleMessage(message)
        }

        // Install method swizzles (Messages only — FaceTime and FindMy don't need them)
        if isMessages {
            installSwizzles()
        }

        client.connect()
    }

    // MARK: - Message Dispatch

    static func handleMessage(_ raw: String) {
        // Handle duplicated JSON (same quirk as the Obj-C version)
        var message = raw
        if let range = message.range(of: "}\n{") {
            message = String(message[message.startIndex...range.lowerBound])
        }

        guard let jsonData = message.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: jsonData) as? [String: Any] else {
            Log.error("handleMessage: failed to parse JSON: \(raw)")
            return
        }

        let action = dict["action"] as? String ?? ""
        let data = dict["data"] as? [String: Any] ?? [:]
        let transaction: String? = {
            guard let val = dict["transactionId"], !(val is NSNull) else { return nil }
            return val as? String
        }()

        switch mode {
        case .messages:
            dispatchMessagesAction(action, data: data, transaction: transaction)
        case .faceTime:
            dispatchFaceTimeAction(action, data: data, transaction: transaction)
        case .findMy:
            dispatchFindMyAction(action, data: data, transaction: transaction)
        }
    }

    // MARK: - Messages Dispatch

    private static func dispatchMessagesAction(_ action: String, data: [String: Any], transaction: String?) {
        switch action {
        // Chat actions
        case "start-typing":
            ChatActions.startTyping(data: data, transaction: transaction)
        case "stop-typing":
            ChatActions.stopTyping(data: data, transaction: transaction)
        case "mark-chat-read":
            ChatActions.markChatRead(data: data, transaction: transaction)
        case "mark-chat-unread":
            ChatActions.markChatUnread(data: data, transaction: transaction)
        case "check-typing-status":
            ChatActions.checkTypingStatus(data: data, transaction: transaction)
        case "set-display-name":
            ChatActions.setDisplayName(data: data, transaction: transaction)
        case "add-participant":
            ChatActions.addParticipant(data: data, transaction: transaction)
        case "remove-participant":
            ChatActions.removeParticipant(data: data, transaction: transaction)
        case "update-chat-pinned":
            ChatActions.updateChatPinned(data: data, transaction: transaction)
        case "create-chat":
            ChatActions.createChat(data: data, transaction: transaction)
        case "delete-chat":
            ChatActions.deleteChat(data: data, transaction: transaction)
        case "leave-chat":
            ChatActions.leaveChat(data: data, transaction: transaction)

        // Message actions
        case "send-message", "send-reaction":
            MessageActions.sendMessage(data: data, transfers: nil, attributedString: nil, transaction: transaction)
        case "edit-message":
            MessageActions.editMessage(data: data, transaction: transaction)
        case "unsend-message":
            MessageActions.unsendMessage(data: data, transaction: transaction)
        case "send-multipart":
            MessageActions.sendMultipart(data: data, transaction: transaction)
        case "delete-message":
            MessageActions.deleteMessage(data: data, transaction: transaction)
        case "notify-anyways":
            MessageActions.notifyAnyways(data: data, transaction: transaction)
        case "search-messages":
            MessageActions.searchMessages(data: data, transaction: transaction)

        // Attachment actions
        case "send-attachment":
            AttachmentActions.sendAttachment(data: data, transaction: transaction)
        case "balloon-bundle-media-path":
            AttachmentActions.balloonBundleMediaPath(data: data, transaction: transaction)
        case "update-group-photo":
            AttachmentActions.updateGroupPhoto(data: data, transaction: transaction)
        case "download-purged-attachment":
            AttachmentActions.downloadPurgedAttachment(data: data, transaction: transaction)

        // Account actions
        case "check-focus-status":
            AccountActions.checkFocusStatus(data: data, transaction: transaction)
        case "check-imessage-availability":
            AccountActions.checkAvailability(data: data, transaction: transaction, service: "iMessage")
        case "check-facetime-availability":
            AccountActions.checkAvailability(data: data, transaction: transaction, service: "FaceTime")
        case "get-account-info":
            AccountActions.getAccountInfo(data: data, transaction: transaction)
        case "modify-active-alias":
            AccountActions.modifyActiveAlias(data: data, transaction: transaction)
        case "should-offer-nickname-sharing":
            AccountActions.shouldOfferNicknameSharing(data: data, transaction: transaction)
        case "share-nickname":
            AccountActions.shareNickname(data: data, transaction: transaction)
        case "get-nickname-info":
            AccountActions.getNicknameInfo(data: data, transaction: transaction)

        // FindMy (friends only — devices require FindMy.app injection)
        case "refresh-findmy-friends":
            FindMyActions.refreshFindMyFriends(data: data, transaction: transaction)

        default:
            Log.info("handleMessage: unimplemented action '\(action)'")
        }
    }

    // MARK: - FaceTime Dispatch

    private static func dispatchFaceTimeAction(_ action: String, data: [String: Any], transaction: String?) {
        switch action {
        case "answer-call":
            FaceTimeActions.answerCall(data: data, transaction: transaction)
        case "leave-call":
            FaceTimeActions.leaveCall(data: data, transaction: transaction)
        case "generate-link":
            FaceTimeActions.generateLink(data: data, transaction: transaction)
        case "admit-pending-member":
            FaceTimeActions.admitPendingMember(data: data, transaction: transaction)
        case "get-active-links":
            FaceTimeActions.getActiveLinks(data: data, transaction: transaction)
        case "invalidate-link":
            FaceTimeActions.invalidateLink(data: data, transaction: transaction)
        default:
            Log.info("handleMessage: unimplemented FaceTime action '\(action)'")
        }
    }

    // MARK: - FindMy Dispatch

    private static func dispatchFindMyAction(_ action: String, data: [String: Any], transaction: String?) {
        switch action {
        case "get-findmy-key":
            FindMyActions.getFindMyKey(data: data, transaction: transaction)
        default:
            Log.info("handleMessage: unimplemented FindMy action '\(action)'")
        }
    }

    // MARK: - Response Helpers

    /// Respond helper: send a transaction response or do nothing if transaction is nil.
    static func respond(transaction: String?, extra: [String: Any] = [:]) {
        guard let transaction = transaction else { return }
        var msg: [String: Any] = ["transactionId": transaction]
        for (k, v) in extra { msg[k] = v }
        tcp?.send(msg)
    }

    /// Send an error response for a transaction.
    static func respondError(transaction: String?, error: String) {
        guard let transaction = transaction else { return }
        tcp?.send(["transactionId": transaction, "error": error])
    }

    /// Send an event (no transaction).
    static func sendEvent(_ event: String, data: [String: Any]? = nil, extra: [String: Any] = [:]) {
        var msg: [String: Any] = ["event": event]
        if let data = data { msg["data"] = data }
        for (k, v) in extra { msg[k] = v }
        tcp?.send(msg)
    }
}

// MARK: - Swizzle Callbacks (called from Swizzles.swift)

/// Called from the IMChat._handleIncomingItem: swizzle.
func handleIncomingItem(_ item: AnyObject, _ chatGuid: NSString) {
    let guid = chatGuid as String

    let incomingTypingSel = NSSelectorFromString("isIncomingTypingMessage")
    if item.responds(to: incomingTypingSel) {
        typealias BoolMethod = @convention(c) (AnyObject, Selector) -> Bool
        let method = unsafeBitCast(item.method(for: incomingTypingSel), to: BoolMethod.self)
        if method(item, incomingTypingSel) {
            IMHelper.sendEvent("started-typing", extra: ["guid": guid])
            return
        }
    }

    if item.responds(to: NSSelectorFromString("isCancelTypingMessage")) {
        let sel = NSSelectorFromString("isCancelTypingMessage")
        typealias BoolMethod = @convention(c) (AnyObject, Selector) -> Bool
        let method = unsafeBitCast(item.method(for: sel), to: BoolMethod.self)
        if method(item, sel) {
            IMHelper.sendEvent("stopped-typing", extra: ["guid": guid])
            return
        }
    }

    // If the item has a message that's not a typing message, also emit stopped-typing
    if item.responds(to: NSSelectorFromString("message")) {
        if let message = item.perform(NSSelectorFromString("message"))?.takeUnretainedValue() {
            let sel = NSSelectorFromString("isTypingMessage")
            if message.responds(to: sel) {
                typealias BoolMethod = @convention(c) (AnyObject, Selector) -> Bool
                let method = unsafeBitCast(message.method(for: sel), to: BoolMethod.self)
                if !method(message, sel) {
                    IMHelper.sendEvent("stopped-typing", extra: ["guid": guid])
                }
            }
        }
    }
}

/// Called from IMFMFSession.didReceiveLocationForHandle: swizzle.
/// On Sequoia, handle is an IMHandle. On Tahoe, handle is an IMFindMyHandle.
func handleLocationUpdateForHandle(_ handle: AnyObject) {
    guard let imfmfSession = getSharedInstance("IMFMFSession") else { return }

    var handleId: String?
    var locationObj: NSObject?

    // macOS 26+ (Tahoe): handle is IMFindMyHandle
    let imFindMyHandleClass: AnyClass? = NSClassFromString("IMFindMyHandle")
    if let cls = imFindMyHandleClass, (handle as AnyObject).isKind(of: cls) {
        handleId = safePerformReturning(handle as! NSObject, selector: "identifier") as? String

        // Use findMyLocationForFindMyHandle: which returns IMFindMyLocation
        let sel = NSSelectorFromString("findMyLocationForFindMyHandle:")
        if imfmfSession.responds(to: sel),
           let imFindMyLoc = safePerformReturning(imfmfSession, selector: "findMyLocationForFindMyHandle:", with: handle) {
            // IMFindMyLocation wraps fmlLocation and/or fmfLocation
            if let fmlLoc = safePerformReturning(imFindMyLoc, selector: "fmlLocation") {
                locationObj = fmlLoc
            } else if let fmfLoc = safePerformReturning(imFindMyLoc, selector: "fmfLocation") {
                locationObj = fmfLoc
            }
        }
    } else {
        // macOS 15 (Sequoia): handle is IMHandle
        handleId = FindMyActions.extractHandleId(handle)

        // Use findMyLocationForHandle:
        let sel = NSSelectorFromString("findMyLocationForHandle:")
        if imfmfSession.responds(to: sel) {
            locationObj = safePerformReturning(imfmfSession, selector: "findMyLocationForHandle:", with: handle)
        }
    }

    guard let handleId = handleId, let locationObj = locationObj else { return }

    guard let locDetails = FindMyActions.serializeLocationObject(locationObj, handleId: handleId) else { return }

    let coords = locDetails["coordinates"] as? [Double] ?? [0, 0]
    let longAddr = locDetails["long_address"]

    if coords[0] == 0 && coords[1] == 0, let addr = longAddr as? String {
        // Geocode the address to get coordinates
        let geocoder = CLGeocoder()
        var mutableDetails = locDetails
        geocoder.geocodeAddressString(addr) { placemarks, _ in
            if let loc = placemarks?.first?.location {
                mutableDetails["coordinates"] = [loc.coordinate.latitude, loc.coordinate.longitude]
            }
            IMHelper.sendEvent("new-findmy-location", data: nil, extra: ["data": [mutableDetails]])
        }
    } else {
        IMHelper.sendEvent("new-findmy-location", data: nil, extra: ["data": [locDetails]])
    }
}

/// Called from IMAccount._registrationStatusChanged: swizzle.
func handleRegistrationStatusChanged(_ notification: NSNotification) {
    guard let account = notification.object as? NSObject,
          let info = notification.userInfo,
          info["__kIMAccountAliasesRemovedKey"] != nil else {
        return
    }

    // Check it's the iMessage service
    if let serviceName = account.value(forKey: "serviceName") as? String,
       serviceName == "iMessage" {
        IMHelper.sendEvent("aliases-removed", data: info as? [String: Any])
    }
}

/// Called from CKConversationListStandardCell.setShowTypingIndicator: swizzle (Tahoe).
func handleTahoeTypingIndicator(_ show: Bool, _ cell: AnyObject) {
    // Navigate: cell.conversation.chat.guid
    guard cell.responds(to: NSSelectorFromString("conversation")),
          let conversation = cell.perform(NSSelectorFromString("conversation"))?.takeUnretainedValue(),
          conversation.responds(to: NSSelectorFromString("chat")),
          let chat = conversation.perform(NSSelectorFromString("chat"))?.takeUnretainedValue(),
          chat.responds(to: NSSelectorFromString("guid")),
          let guid = chat.perform(NSSelectorFromString("guid"))?.takeUnretainedValue() as? String else {
        return
    }

    let event = show ? "started-typing" : "stopped-typing"
    IMHelper.sendEvent(event, extra: ["guid": guid])
    Log.info("\(guid) \(event) (Tahoe)")
}
