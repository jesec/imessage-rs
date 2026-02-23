import Foundation

enum FaceTimeActions {

    // MARK: - Call Observer Registration

    private static var videoObserver: NSObjectProtocol?
    private static var callObserver: NSObjectProtocol?

    static func registerCallObservers() {
        videoObserver = NotificationCenter.default.addObserver(
            forName: NSNotification.Name("TUCallCenterVideoCallStatusChangedNotification"),
            object: nil,
            queue: .main
        ) { notification in
            callStatusChanged(notification)
        }
        callObserver = NotificationCenter.default.addObserver(
            forName: NSNotification.Name("TUCallCenterCallStatusChangedNotification"),
            object: nil,
            queue: .main
        ) { notification in
            callStatusChanged(notification)
        }
        Log.info("FaceTime call listeners registered")
    }

    // MARK: - Call Status Changed

    private static func callStatusChanged(_ notification: Notification) {
        guard let call = notification.object as? NSObject else { return }

        let audioMode = safePerformReturning(call, selector: "audioMode")
        let callStatus = callInt(call, selector: "callStatus")
        let callUUID = safePerformReturning(call, selector: "callUUID")
        let isConversation = callBool(call, selector: "isConversation")
        let disconnectedReason = callInt(call, selector: "disconnectedReason")
        let endedError = safePerformReturning(call, selector: "endedErrorString") as? String
        let endedReason = safePerformReturning(call, selector: "endedReasonString") as? String
        let isSendingAudio = callBool(call, selector: "isSendingAudio")
        let isSendingVideo = callBool(call, selector: "isSendingVideo")
        let isOutgoing = callBool(call, selector: "isOutgoing")

        // Get handle dictionary
        var handleDict: Any = NSNull()
        if let handle = safePerformReturning(call, selector: "handle") {
            if let dict = safePerformReturning(handle, selector: "dictionaryRepresentation") {
                handleDict = dict
            }
        }

        let data: [String: Any] = [
            "audio_mode": audioMode ?? NSNull(),
            "call_status": callStatus,
            "call_uuid": callUUID ?? NSNull(),
            "is_conversation": isConversation,
            "disconnected_reason": disconnectedReason,
            "ended_error": endedError ?? NSNull(),
            "ended_reason": endedReason ?? NSNull(),
            "handle": handleDict,
            "is_sending_audio": isSendingAudio,
            "is_sending_video": isSendingVideo,
            "is_outgoing": isOutgoing,
        ]

        IMHelper.sendEvent("facetime-call-status-changed", data: data)
    }

    // MARK: - Answer Call

    static func answerCall(data: [String: Any], transaction: String?) {
        guard let callUUID = data["callUUID"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide a call UUID!")
            return
        }

        guard let callCenter = getSharedInstance("TUCallCenter"),
              let call = safePerformReturning(callCenter, selector: "callWithCallUUID:", with: callUUID) else {
            IMHelper.respondError(transaction: transaction, error: "No call found with the given UUID!")
            return
        }

        let status = callInt(call, selector: "callStatus")
        if status != 4 {
            IMHelper.respondError(transaction: transaction, error: "Call is not waiting to be answered!")
            return
        }

        safePerform(callCenter, selector: "answerOrJoinCall:", with: call)
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Leave Call

    static func leaveCall(data: [String: Any], transaction: String?) {
        guard let callUUID = data["callUUID"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide a call UUID!")
            return
        }

        guard let callCenter = getSharedInstance("TUCallCenter"),
              let call = safePerformReturning(callCenter, selector: "callWithCallUUID:", with: callUUID) else {
            IMHelper.respondError(transaction: transaction, error: "No call found with the given UUID!")
            return
        }

        let status = callInt(call, selector: "callStatus")
        if status != 1 {
            IMHelper.respondError(transaction: transaction, error: "Call is not waiting to be left!")
            return
        }

        safePerform(callCenter, selector: "disconnectCall:", with: call)
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Generate Link

    static func generateLink(data: [String: Any], transaction: String?) {
        guard let managerClass = NSClassFromString("TUConversationManagerXPCClient") as? NSObject.Type else {
            IMHelper.respondError(transaction: transaction, error: "TUConversationManagerXPCClient not available!")
            return
        }
        let manager = managerClass.init()

        let callUUID = data["callUUID"]
        if let uuid = callUUID as? String, !uuid.isEmpty {
            // Generate link for existing call
            guard let callCenter = getSharedInstance("TUCallCenter"),
                  let call = safePerformReturning(callCenter, selector: "callWithCallUUID:", with: uuid) else {
                IMHelper.respondError(transaction: transaction, error: "No call found with the given UUID!")
                return
            }

            guard let convo = safePerformReturning(callCenter, selector: "activeConversationForCall:", with: call) else {
                IMHelper.respondError(transaction: transaction, error: "No active conversation for call!")
                return
            }

            let sel = NSSelectorFromString("generateLinkForConversation:completionHandler:")
            guard manager.responds(to: sel) else {
                IMHelper.respondError(transaction: transaction, error: "generateLinkForConversation selector not found!")
                return
            }
            typealias GenMethod = @convention(c) (NSObject, Selector, NSObject, @escaping @convention(block) (AnyObject?, AnyObject?) -> Void) -> Void
            let imp = manager.method(for: sel)
            let fn = unsafeBitCast(imp, to: GenMethod.self)
            fn(manager, sel, convo) { link, error in
                if let err = error as? NSError {
                    IMHelper.respondError(transaction: transaction, error: err.localizedDescription)
                } else if let link = link as? NSObject,
                          let url = safePerformReturning(link, selector: "URL") as? URL {
                    IMHelper.respond(transaction: transaction, extra: ["url": url.absoluteString])
                } else {
                    IMHelper.respondError(transaction: transaction, error: "FaceTime returned nil link for conversation")
                }
            }
        } else {
            // Generate a new standalone link
            let sel = NSSelectorFromString("generateLinkWithInvitedMemberHandles:linkLifetimeScope:completionHandler:")
            guard manager.responds(to: sel) else {
                IMHelper.respondError(transaction: transaction, error: "generateLink selector not found!")
                return
            }
            typealias GenMethod = @convention(c) (NSObject, Selector, NSArray, Int, @escaping @convention(block) (AnyObject?, AnyObject?) -> Void) -> Void
            let imp = manager.method(for: sel)
            let fn = unsafeBitCast(imp, to: GenMethod.self)
            fn(manager, sel, [] as NSArray, 0) { link, error in
                if let err = error as? NSError {
                    IMHelper.respondError(transaction: transaction, error: err.localizedDescription)
                } else if let link = link as? NSObject,
                          let url = safePerformReturning(link, selector: "URL") as? URL {
                    IMHelper.respond(transaction: transaction, extra: ["url": url.absoluteString])
                } else {
                    IMHelper.respondError(transaction: transaction, error: "FaceTime returned nil link")
                }
            }
        }
    }

    // MARK: - Admit Pending Member

    static func admitPendingMember(data: [String: Any], transaction: String?) {
        guard let conversationUUID = data["conversationUUID"] as? String,
              let handleUUID = data["handleUUID"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide conversationUUID and handleUUID!")
            return
        }

        guard let managerClass = NSClassFromString("TUConversationManager") as? NSObject.Type else {
            IMHelper.respondError(transaction: transaction, error: "TUConversationManager not available!")
            return
        }
        let manager = managerClass.init()

        guard let conversations = safePerformReturning(manager, selector: "activeConversations") as? [NSObject] else {
            IMHelper.respondError(transaction: transaction, error: "No active conversations!")
            return
        }

        var targetConvo: NSObject?
        for convo in conversations {
            if let groupUUID = safePerformReturning(convo, selector: "groupUUID") as? NSUUID,
               groupUUID.uuidString == conversationUUID {
                targetConvo = convo
                break
            }
        }

        guard let convo = targetConvo else {
            IMHelper.respondError(transaction: transaction, error: "Conversation not found!")
            return
        }

        guard let pendingMembers = safePerformReturning(convo, selector: "pendingMembers") as? [NSObject] else { return }

        for member in pendingMembers {
            if let handle = safePerformReturning(member, selector: "handle"),
               let value = safePerformReturning(handle, selector: "value") as? String,
               value == handleUUID {
                let sel = NSSelectorFromString("approvePendingMember:forConversation:")
                if manager.responds(to: sel) {
                    typealias ApproveMethod = @convention(c) (NSObject, Selector, NSObject, NSObject) -> Void
                    let imp = manager.method(for: sel)
                    let fn = unsafeBitCast(imp, to: ApproveMethod.self)
                    fn(manager, sel, member, convo)
                    IMHelper.respond(transaction: transaction)
                }
                return
            }
        }

        IMHelper.respondError(transaction: transaction, error: "Pending member not found!")
    }

    // MARK: - Get Active Links

    static func getActiveLinks(data: [String: Any], transaction: String?) {
        guard let managerClass = NSClassFromString("TUConversationManager") as? NSObject.Type else {
            IMHelper.respondError(transaction: transaction, error: "TUConversationManager not available!")
            return
        }
        let manager = managerClass.init()

        guard let linksSet = safePerformReturning(manager, selector: "activatedConversationLinks") as? NSSet else {
            IMHelper.respond(transaction: transaction, extra: ["data": ["links": [Any]()]])
            return
        }

        var linksArray: [[String: Any]] = []
        for linkObj in linksSet {
            guard let link = linkObj as? NSObject else { continue }

            let url = (safePerformReturning(link, selector: "URL") as? URL)?.absoluteString
            let creationDate = (safePerformReturning(link, selector: "creationDate") as? Date)
                .map { $0.timeIntervalSince1970 * 1000 }
            let expirationDate = (safePerformReturning(link, selector: "expirationDate") as? Date)
                .map { $0.timeIntervalSince1970 * 1000 }
            let groupUUID = (safePerformReturning(link, selector: "groupUUID") as? NSUUID)?.uuidString
            let linkName = safePerformReturning(link, selector: "linkName") as? String

            // Get invited handles
            var handleArray: [Any] = []
            if let handlesSet = safePerformReturning(link, selector: "invitedMemberHandles") as? NSSet {
                for handleObj in handlesSet {
                    if let handle = handleObj as? NSObject,
                       let dict = safePerformReturning(handle, selector: "dictionaryRepresentation") {
                        handleArray.append(dict)
                    }
                }
            }

            let linkData: [String: Any] = [
                "url": url ?? NSNull(),
                "creation_date": creationDate ?? NSNull(),
                "expiration_date": expirationDate ?? NSNull(),
                "group_uuid": groupUUID ?? NSNull(),
                "name": linkName ?? NSNull(),
                "handles": handleArray,
            ]
            linksArray.append(linkData)
        }

        IMHelper.respond(transaction: transaction, extra: ["data": ["links": linksArray]])
    }

    // MARK: - Invalidate Link

    static func invalidateLink(data: [String: Any], transaction: String?) {
        guard let urlString = data["url"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide a URL!")
            return
        }

        guard let managerClass = NSClassFromString("TUConversationManager") as? NSObject.Type else {
            IMHelper.respondError(transaction: transaction, error: "TUConversationManager not available!")
            return
        }
        let manager = managerClass.init()

        guard let linksSet = safePerformReturning(manager, selector: "activatedConversationLinks") as? NSSet else { return }

        for linkObj in linksSet {
            guard let link = linkObj as? NSObject,
                  let url = safePerformReturning(link, selector: "URL") as? URL,
                  url.absoluteString == urlString else { continue }

            guard let xpcClass = NSClassFromString("TUConversationManagerXPCClient") as? NSObject.Type else { break }
            let xpcManager = xpcClass.init()

            let sel = NSSelectorFromString("invalidateLink:completionHandler:")
            if xpcManager.responds(to: sel) {
                typealias InvalidateMethod = @convention(c) (NSObject, Selector, NSObject, @escaping @convention(block) (Int8, AnyObject?) -> Void) -> Void
                let imp = xpcManager.method(for: sel)
                let fn = unsafeBitCast(imp, to: InvalidateMethod.self)
                fn(xpcManager, sel, link) { _, _ in
                    IMHelper.respond(transaction: transaction)
                }
            }
            return
        }

        IMHelper.respondError(transaction: transaction, error: "Link not found!")
    }
}
