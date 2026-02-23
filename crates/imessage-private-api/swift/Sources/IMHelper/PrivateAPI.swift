import Foundation

// MARK: - Runtime Helpers

/// Get the shared instance of a private framework singleton.
func getSharedInstance(_ className: String) -> NSObject? {
    guard let cls = NSClassFromString(className) as? NSObject.Type else { return nil }
    let sel = NSSelectorFromString("sharedInstance")
    guard cls.responds(to: sel) else { return nil }
    return cls.perform(sel)?.takeUnretainedValue() as? NSObject
}

/// Safely call a void method with one object argument using performSelector.
func safePerform(_ obj: NSObject, selector: String, with arg: Any? = nil) {
    let sel = NSSelectorFromString(selector)
    guard obj.responds(to: sel) else {
        Log.error("safePerform: \(type(of: obj)) does not respond to \(selector)")
        return
    }
    if let arg = arg {
        obj.perform(sel, with: arg)
    } else {
        obj.perform(sel)
    }
}

/// Call a method that returns an object, with one argument.
func safePerformReturning(_ obj: NSObject, selector: String, with arg: Any? = nil) -> NSObject? {
    let sel = NSSelectorFromString(selector)
    guard obj.responds(to: sel) else { return nil }
    let result: Unmanaged<AnyObject>?
    if let arg = arg {
        result = obj.perform(sel, with: arg)
    } else {
        result = obj.perform(sel)
    }
    return result?.takeUnretainedValue() as? NSObject
}

/// Call a method returning Bool using IMP casting.
func callBool(_ obj: NSObject, selector: String) -> Bool {
    let sel = NSSelectorFromString(selector)
    guard obj.responds(to: sel) else { return false }
    typealias BoolMethod = @convention(c) (NSObject, Selector) -> Bool
    let imp = obj.method(for: sel)
    let fn = unsafeBitCast(imp, to: BoolMethod.self)
    return fn(obj, sel)
}

/// Call a method returning Int using IMP casting.
func callInt(_ obj: NSObject, selector: String) -> Int {
    let sel = NSSelectorFromString(selector)
    guard obj.responds(to: sel) else { return 0 }
    typealias IntMethod = @convention(c) (NSObject, Selector) -> Int
    let imp = obj.method(for: sel)
    let fn = unsafeBitCast(imp, to: IntMethod.self)
    return fn(obj, sel)
}

/// Allocate an instance of a class without calling init.
/// Use this instead of `.init()` for IMCore classes that crash on the no-args init
/// (e.g. IMEmojiTapback, IMStickerTapback, IMTapbackSender).
func runtimeAlloc(_ cls: NSObject.Type) -> NSObject? {
    cls.perform(NSSelectorFromString("alloc"))?.takeUnretainedValue() as? NSObject
}

// MARK: - Chat Helpers

/// Look up an IMChat by GUID, with Tahoe "any;-;" prefix fallback.
func getChat(guid: String?, transaction: String?) -> NSObject? {
    guard let guid = guid else {
        IMHelper.respondError(transaction: transaction, error: "Provide a chat GUID!")
        return nil
    }

    guard let registry = getSharedInstance("IMChatRegistry") else {
        IMHelper.respondError(transaction: transaction, error: "IMChatRegistry not available!")
        return nil
    }

    // Try direct lookup
    if let chat = safePerformReturning(registry, selector: "existingChatWithGUID:", with: guid) {
        return chat
    }

    // Tahoe fix: try "any" service prefix
    var tahoeGuid: String?
    if guid.hasPrefix("iMessage;-;") {
        tahoeGuid = guid.replacingOccurrences(of: "iMessage;-;", with: "any;-;")
    } else if guid.hasPrefix("SMS;-;") {
        tahoeGuid = guid.replacingOccurrences(of: "SMS;-;", with: "any;-;")
    }

    if let tahoeGuid = tahoeGuid,
       let chat = safePerformReturning(registry, selector: "existingChatWithGUID:", with: tahoeGuid) {
        return chat
    }

    IMHelper.respondError(transaction: transaction, error: "Chat does not exist!")
    return nil
}

/// Get an IMHandle from the active iMessage account.
func getIMHandle(address: String) -> NSObject? {
    guard let accountController = getSharedInstance("IMAccountController"),
          let account = safePerformReturning(accountController, selector: "activeIMessageAccount"),
          let handle = safePerformReturning(account, selector: "imHandleWithID:", with: address) else {
        return nil
    }
    return handle
}

/// Get an IMHandle from the active SMS account.
func getSMSHandle(address: String) -> NSObject? {
    guard let accountController = getSharedInstance("IMAccountController"),
          let account = safePerformReturning(accountController, selector: "activeSMSAccount"),
          let handle = safePerformReturning(account, selector: "imHandleWithID:", with: address) else {
        return nil
    }
    return handle
}

/// Load a message by GUID from IMChatHistoryController.
func getMessageItem(guid: String, completion: @escaping (NSObject?) -> Void) {
    guard let historyController = getSharedInstance("IMChatHistoryController") else {
        completion(nil)
        return
    }

    let sel = NSSelectorFromString("loadMessageWithGUID:completionBlock:")
    guard historyController.responds(to: sel) else {
        completion(nil)
        return
    }

    typealias LoadMethod = @convention(c) (NSObject, Selector, NSString, @escaping @convention(block) (AnyObject?) -> Void) -> Void
    let imp = historyController.method(for: sel)
    let fn = unsafeBitCast(imp, to: LoadMethod.self)
    fn(historyController, sel, guid as NSString) { message in
        completion(message as? NSObject)
    }
}

/// Find an IMMessagePartChatItem at the given part index, navigating aggregate attachments.
func findPartChatItem(items: Any, partIndex: Int) -> NSObject? {
    if let itemArray = items as? [NSObject] {
        let aggregateClass: AnyClass? = NSClassFromString("IMAggregateAttachmentMessagePartChatItem")

        for item in itemArray {
            if let aggCls = aggregateClass, item.isKind(of: aggCls) {
                // Check sub-parts of the aggregate
                if let parts = safePerformReturning(item, selector: "aggregateAttachmentParts") as? [NSObject] {
                    for subItem in parts {
                        if callInt(subItem, selector: "index") == partIndex {
                            return subItem
                        }
                    }
                }
            } else {
                if callInt(item, selector: "index") == partIndex {
                    return item
                }
            }
        }
        return nil
    } else if let single = items as? NSObject {
        return single
    }
    return nil
}

// MARK: - Reaction Helpers

/// Map reaction type string to the IMCore long long code.
func parseReactionType(_ type: String) -> Int64 {
    switch type.lowercased() {
    case "love":       return 2000
    case "like":       return 2001
    case "dislike":    return 2002
    case "laugh":      return 2003
    case "emphasize":  return 2004
    case "question":   return 2005
    case "-love":      return 3000
    case "-like":      return 3001
    case "-dislike":   return 3002
    case "-laugh":     return 3003
    case "-emphasize": return 3004
    case "-question":  return 3005
    default:           return 0
    }
}

/// Map reaction type string to human-readable verb for tapback summary.
func reactionToVerb(_ type: String) -> String {
    switch type.lowercased() {
    case "love":       return "Loved "
    case "like":       return "Liked "
    case "dislike":    return "Disliked "
    case "laugh":      return "Laughed at "
    case "emphasize":  return "Emphasized "
    case "question":   return "Questioned "
    case "-love":      return "Removed a heart from "
    case "-like":      return "Removed a like from "
    case "-dislike":   return "Removed a dislike from "
    case "-laugh":     return "Removed a laugh from "
    case "-emphasize": return "Removed an exclamation from "
    case "-question":  return "Removed a question mark from "
    default:           return ""
    }
}

// MARK: - Emoji & Sticker Tapback Helpers

/// Send an emoji tapback using IMEmojiTapback + IMTapbackSender.
func sendEmojiTapback(emoji: String, isRemoved: Bool, chat: NSObject, item: NSObject?, message: NSObject, transaction: String?) {
    guard let EmojiTapbackClass = NSClassFromString("IMEmojiTapback") as? NSObject.Type else {
        IMHelper.respondError(transaction: transaction, error: "IMEmojiTapback class not available")
        return
    }

    // Use alloc + designated initializer (NOT .init() which crashes because
    // IMEmojiTapback doesn't support the no-args init)
    guard let rawObj = runtimeAlloc(EmojiTapbackClass) else {
        IMHelper.respondError(transaction: transaction, error: "IMEmojiTapback alloc failed")
        return
    }

    let initSel = NSSelectorFromString("initWithEmoji:isRemoved:")
    guard rawObj.responds(to: initSel) else {
        IMHelper.respondError(transaction: transaction, error: "IMEmojiTapback does not respond to initWithEmoji:isRemoved:")
        return
    }
    typealias TapbackInit = @convention(c) (NSObject, Selector, NSString, Bool) -> NSObject
    let initImp = rawObj.method(for: initSel)
    let initFn = unsafeBitCast(initImp, to: TapbackInit.self)
    let tapback = initFn(rawObj, initSel, emoji as NSString, isRemoved)

    sendTapback(tapback, chat: chat, item: item, message: message, transaction: transaction)
}

/// Send a sticker tapback using IMStickerTapback + IMTapbackSender.
func sendStickerTapback(stickerPath: String, isRemoved: Bool, chat: NSObject, item: NSObject?, message: NSObject, transaction: String?) {
    if isRemoved {
        // For sticker removal, we still use IMStickerTapback but without a file transfer
        guard let StickerTapbackClass = NSClassFromString("IMStickerTapback") as? NSObject.Type else {
            IMHelper.respondError(transaction: transaction, error: "IMStickerTapback class not available")
            return
        }

        guard let rawObj = runtimeAlloc(StickerTapbackClass) else {
            IMHelper.respondError(transaction: transaction, error: "IMStickerTapback alloc failed")
            return
        }
        let initSel = NSSelectorFromString("initWithTransferGUID:isRemoved:")
        guard rawObj.responds(to: initSel) else {
            IMHelper.respondError(transaction: transaction, error: "IMStickerTapback does not respond to initWithTransferGUID:isRemoved:")
            return
        }
        typealias TapbackInit = @convention(c) (NSObject, Selector, NSString, Bool) -> NSObject
        let initImp = rawObj.method(for: initSel)
        let initFn = unsafeBitCast(initImp, to: TapbackInit.self)
        let tapback = initFn(rawObj, initSel, "" as NSString, true)

        sendTapback(tapback, chat: chat, item: item, message: message, transaction: transaction)
        return
    }

    // Prepare file transfer for the sticker image
    let fileUrl = URL(fileURLWithPath: stickerPath)
    guard let transfer = AttachmentActions.prepareFileTransfer(url: fileUrl, filename: fileUrl.lastPathComponent) else {
        IMHelper.respondError(transaction: transaction, error: "Failed to prepare sticker file transfer")
        return
    }
    let transferGuid = transfer.value(forKey: "guid") as? String ?? ""

    guard let StickerTapbackClass = NSClassFromString("IMStickerTapback") as? NSObject.Type else {
        IMHelper.respondError(transaction: transaction, error: "IMStickerTapback class not available")
        return
    }

    guard let rawObj = runtimeAlloc(StickerTapbackClass) else {
        IMHelper.respondError(transaction: transaction, error: "IMStickerTapback alloc failed")
        return
    }
    let initSel = NSSelectorFromString("initWithTransferGUID:isRemoved:")
    guard rawObj.responds(to: initSel) else {
        IMHelper.respondError(transaction: transaction, error: "IMStickerTapback does not respond to initWithTransferGUID:isRemoved:")
        return
    }
    typealias TapbackInit = @convention(c) (NSObject, Selector, NSString, Bool) -> NSObject
    let initImp = rawObj.method(for: initSel)
    let initFn = unsafeBitCast(initImp, to: TapbackInit.self)
    let tapback = initFn(rawObj, initSel, transferGuid as NSString, false)

    sendTapback(tapback, chat: chat, item: item, message: message, transaction: transaction)
}

/// Send a tapback object using IMTapbackSender.
/// Works for all tapback types (classic IMClassicTapback, emoji IMEmojiTapback, sticker IMStickerTapback).
func sendTapback(_ tapback: NSObject, chat: NSObject, item: NSObject?, message: NSObject, transaction: String?) {
    guard let SenderClass = NSClassFromString("IMTapbackSender") as? NSObject.Type else {
        IMHelper.respondError(transaction: transaction, error: "IMTapbackSender not available")
        return
    }

    // Use alloc instead of .init() to avoid crashing on classes that require designated initializers
    guard let sender = runtimeAlloc(SenderClass) else {
        IMHelper.respondError(transaction: transaction, error: "IMTapbackSender alloc failed")
        return
    }

    if let item = item {
        // Convenience init with chat item — handles GUID/range/summary automatically
        let sel = NSSelectorFromString("initWithTapback:chat:messagePartChatItem:")
        if sender.responds(to: sel) {
            typealias SenderInit = @convention(c) (NSObject, Selector, NSObject, NSObject, NSObject) -> NSObject
            let imp = sender.method(for: sel)
            let fn = unsafeBitCast(imp, to: SenderInit.self)
            let senderObj = fn(sender, sel, tapback, chat, item)
            senderObj.perform(NSSelectorFromString("send"))
        } else {
            // Fallback to full init
            sendTapbackWithFullInit(tapback, sender: sender, chat: chat, message: message, transaction: transaction)
            return
        }
    } else {
        sendTapbackWithFullInit(tapback, sender: sender, chat: chat, message: message, transaction: transaction)
        return
    }

    let lastGuid = safePerformReturning(chat, selector: "lastSentMessage")?.value(forKey: "guid") as? String ?? ""
    IMHelper.respond(transaction: transaction, extra: ["identifier": lastGuid])
}

/// Fallback: full init with explicit GUID/range for IMTapbackSender.
private func sendTapbackWithFullInit(_ tapback: NSObject, sender: NSObject, chat: NSObject, message: NSObject, transaction: String?) {
    let messageGuid = message.value(forKey: "guid") as? String ?? ""
    let sel = NSSelectorFromString("initWithTapback:chat:messageGUID:messagePartRange:messageSummaryInfo:threadIdentifier:")
    guard sender.responds(to: sel) else {
        IMHelper.respondError(transaction: transaction, error: "IMTapbackSender does not respond to full init")
        return
    }
    typealias SenderInit = @convention(c) (NSObject, Selector, NSObject, NSObject, NSString, NSRange, NSDictionary?, NSString?) -> NSObject
    let imp = sender.method(for: sel)
    let fn = unsafeBitCast(imp, to: SenderInit.self)
    let senderObj = fn(sender, sel, tapback, chat, messageGuid as NSString, NSRange(location: 0, length: 0), nil, nil)
    senderObj.perform(NSSelectorFromString("send"))

    let lastGuid = safePerformReturning(chat, selector: "lastSentMessage")?.value(forKey: "guid") as? String ?? ""
    IMHelper.respond(transaction: transaction, extra: ["identifier": lastGuid])
}
