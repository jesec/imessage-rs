import Foundation

enum ChatActions {

    // MARK: - Typing

    static func startTyping(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        safePerform(chat, selector: "setLocalUserIsTyping:", with: NSNumber(value: true))
        IMHelper.respond(transaction: transaction)
    }

    static func stopTyping(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        safePerform(chat, selector: "setLocalUserIsTyping:", with: NSNumber(value: false))
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Read State

    static func markChatRead(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        safePerform(chat, selector: "markAllMessagesAsRead")
        IMHelper.respond(transaction: transaction)
    }

    static func markChatUnread(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        safePerform(chat, selector: "markLastMessageAsUnread")
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Typing Status Check

    static func checkTypingStatus(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: nil) else { return }
        let guid = chat.value(forKey: "guid") as? String ?? ""

        // Check chat.lastIncomingMessage.isTypingMessage
        if let lastMsg = safePerformReturning(chat, selector: "lastIncomingMessage") {
            if callBool(lastMsg, selector: "isTypingMessage") {
                IMHelper.sendEvent("started-typing", extra: ["guid": guid])
                return
            }
        }
        IMHelper.sendEvent("stopped-typing", extra: ["guid": guid])
    }

    // MARK: - Display Name

    static func setDisplayName(data: [String: Any], transaction: String?) {
        guard let newName = data["newName"] as? String, !newName.isEmpty else {
            IMHelper.respondError(transaction: transaction, error: "Provide a new name for the chat!")
            return
        }
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        safePerform(chat, selector: "_setDisplayName:", with: newName)
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Participants

    static func addParticipant(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }

        guard let address = data["address"] as? String, !address.isEmpty else {
            IMHelper.respondError(transaction: transaction, error: "Provide an address to add!")
            return
        }

        guard let handle = getIMHandle(address: address) else {
            IMHelper.respondError(transaction: transaction, error: "Failed to add address to chat!")
            return
        }

        // Check canAddParticipant: (requires handle argument via IMP cast)
        let canAddSel = NSSelectorFromString("canAddParticipant:")
        if chat.responds(to: canAddSel) {
            typealias CanAddMethod = @convention(c) (NSObject, Selector, NSObject) -> Bool
            let canAddImp = chat.method(for: canAddSel)
            let canAddFn = unsafeBitCast(canAddImp, to: CanAddMethod.self)
            if !canAddFn(chat, canAddSel, handle) {
                IMHelper.respondError(transaction: transaction, error: "Cannot add participant to this chat!")
                return
            }
        }

        let sel = NSSelectorFromString("inviteParticipants:reason:")
        if chat.responds(to: sel) {
            typealias InviteMethod = @convention(c) (NSObject, Selector, NSArray, Any?) -> Void
            let imp = chat.method(for: sel)
            let fn = unsafeBitCast(imp, to: InviteMethod.self)
            fn(chat, sel, [handle] as NSArray, nil)
            IMHelper.respond(transaction: transaction)
        } else {
            IMHelper.respondError(transaction: transaction, error: "Failed to add address to chat!")
        }
    }

    static func removeParticipant(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }

        guard let address = data["address"] as? String, !address.isEmpty else {
            IMHelper.respondError(transaction: transaction, error: "Provide an address to remove!")
            return
        }

        guard let handle = getIMHandle(address: address) else {
            IMHelper.respondError(transaction: transaction, error: "Failed to remove address from chat!")
            return
        }

        let sel = NSSelectorFromString("removeParticipants:reason:")
        if chat.responds(to: sel) {
            typealias RemoveMethod = @convention(c) (NSObject, Selector, NSArray, Any?) -> Void
            let imp = chat.method(for: sel)
            let fn = unsafeBitCast(imp, to: RemoveMethod.self)
            fn(chat, sel, [handle] as NSArray, nil)
            IMHelper.respond(transaction: transaction)
        } else {
            IMHelper.respondError(transaction: transaction, error: "Failed to remove address from chat!")
        }
    }

    // MARK: - Pin/Unpin

    static func updateChatPinned(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let controller = getSharedInstance("IMPinnedConversationsController") else { return }

        let isPinned = callBool(chat, selector: "isPinned")
        let pinId = safePerformReturning(chat, selector: "pinningIdentifier")

        guard let identifierSet = safePerformReturning(controller, selector: "pinnedConversationIdentifierSet"),
              let arr = safePerformReturning(identifierSet, selector: "array") as? [Any] else { return }

        var chatArr = arr as [Any]
        if !isPinned {
            if let pinId = pinId { chatArr.append(pinId) }
        } else {
            chatArr.removeAll { ($0 as AnyObject).isEqual(pinId) }
        }

        let sel = NSSelectorFromString("setPinnedConversationIdentifiers:withUpdateReason:")
        if controller.responds(to: sel) {
            typealias SetPinMethod = @convention(c) (NSObject, Selector, NSArray, NSString) -> Void
            let imp = controller.method(for: sel)
            let fn = unsafeBitCast(imp, to: SetPinMethod.self)
            fn(controller, sel, chatArr as NSArray, "contextMenu" as NSString)
        }

        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Create Chat

    static func createChat(data: [String: Any], transaction: String?) {
        guard let addresses = data["addresses"] as? [String] else {
            IMHelper.respondError(transaction: transaction, error: "Provide addresses!")
            return
        }

        let service = data["service"] as? String ?? "iMessage"
        var handles: [NSObject] = []
        var failed = false

        for addr in addresses {
            let handle: NSObject?
            if service == "iMessage" {
                handle = getIMHandle(address: addr)
            } else {
                handle = getSMSHandle(address: addr)
            }

            if let handle = handle {
                handles.append(handle)
            } else {
                failed = true
                break
            }
        }

        if failed {
            IMHelper.respondError(transaction: transaction, error: "Failed to find all handles for specified service!")
            return
        }

        guard let registry = getSharedInstance("IMChatRegistry") else {
            IMHelper.respondError(transaction: transaction, error: "IMChatRegistry not available!")
            return
        }

        let chat: NSObject?
        if handles.count > 1 {
            chat = safePerformReturning(registry, selector: "chatForIMHandles:", with: handles as NSArray)
        } else {
            chat = safePerformReturning(registry, selector: "chatForIMHandle:", with: handles[0])
        }

        guard let chat = chat, let chatGuid = chat.value(forKey: "guid") as? String else {
            IMHelper.respondError(transaction: transaction, error: "Failed to create chat!")
            return
        }

        var mutableData = data
        mutableData["chatGuid"] = chatGuid
        MessageActions.sendMessage(data: mutableData, transfers: nil, attributedString: nil, transaction: transaction)
    }

    // MARK: - Delete / Leave

    static func deleteChat(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let registry = getSharedInstance("IMChatRegistry") else { return }
        safePerform(registry, selector: "_chat_remove:", with: chat)
        IMHelper.respond(transaction: transaction)
    }

    static func leaveChat(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }

        let leaveSel = NSSelectorFromString("leave")
        let leaveGroupSel = NSSelectorFromString("leaveiMessageGroup")

        if chat.responds(to: leaveSel) {
            chat.perform(leaveSel)
        } else if chat.responds(to: leaveGroupSel) {
            chat.perform(leaveGroupSel)
        }

        IMHelper.respond(transaction: transaction)
    }
}
