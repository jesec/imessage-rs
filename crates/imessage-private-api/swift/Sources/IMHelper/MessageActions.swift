import Foundation
import CoreSpotlight

enum MessageActions {

    // MARK: - Send Message / Reaction

    /// Main send method — matches the Obj-C `+sendMessage:transfers:attributedString:transaction:`.
    static func sendMessage(data: [String: Any], transfers: [String]?, attributedString: NSMutableAttributedString?, transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }

        // Build attributed string if not already provided (multipart provides one)
        var attrStr = attributedString
        if attrStr == nil {
            let message = data["message"] as? String ?? "TEMP"
            let formatting = data["textFormatting"] as? [[String: Any]]
            attrStr = applyTextFormatting(formatting, toMessage: message)
        }

        // Subject
        var subjectStr: NSMutableAttributedString?
        if let subject = data["subject"] as? String, !subject.isEmpty {
            subjectStr = NSMutableAttributedString(string: subject)
        }

        // Effect
        var effectId: String?
        if let eff = data["effectId"] as? String, !eff.isEmpty {
            effectId = eff
        }

        // Flags
        let isAudioMessage = (data["isAudioMessage"] as? Int ?? 0) == 1
        let ddScan = (data["ddScan"] as? Int ?? 0) == 1

        // Build and send the message
        let createAndSend: (NSAttributedString?, NSAttributedString?, String?, String?, String?, Int64?, NSRange, [String: Any]?, [String]?, Bool, Bool) -> Void = {
            message, subject, effectId, threadId, assocGuid, reaction, range, summaryInfo, transferGUIDs, isAudio, ddScan in

            guard let messageClass = NSClassFromString("IMMessage") as? NSObject.Type else { return }
            var messageToSend = messageClass.init()

            if reaction == nil {
                // Regular message
                let flags: Int64 = isAudio ? 0x300005 : (subject != nil ? 0x10000d : 0x100005)
                let sel = NSSelectorFromString("initWithSender:time:text:messageSubject:fileTransferGUIDs:flags:error:guid:subject:balloonBundleID:payloadData:expressiveSendStyleID:")
                if messageToSend.responds(to: sel) {
                    typealias InitMethod = @convention(c) (NSObject, Selector, Any?, Any?, NSAttributedString?, NSAttributedString?, NSArray?, Int64, Any?, Any?, Any?, Any?, Any?, NSString?) -> NSObject
                    let imp = messageToSend.method(for: sel)
                    let fn = unsafeBitCast(imp, to: InitMethod.self)
                    messageToSend = fn(messageToSend, sel, nil, nil, message, subject, transferGUIDs as NSArray?, flags, nil, nil, nil, nil, nil, effectId as NSString?)
                }
                if let threadId = threadId {
                    messageToSend.setValue(threadId, forKey: "threadIdentifier")
                }
            } else {
                // Reaction/tapback
                let sel = NSSelectorFromString("initWithSender:time:text:messageSubject:fileTransferGUIDs:flags:error:guid:subject:associatedMessageGUID:associatedMessageType:associatedMessageRange:messageSummaryInfo:")
                if messageToSend.responds(to: sel) {
                    typealias InitMethod = @convention(c) (NSObject, Selector, Any?, Any?, NSAttributedString?, NSAttributedString?, Any?, Int64, Any?, Any?, Any?, NSString?, Int64, NSRange, NSDictionary?) -> NSObject
                    let imp = messageToSend.method(for: sel)
                    let fn = unsafeBitCast(imp, to: InitMethod.self)
                    messageToSend = fn(messageToSend, sel, nil, nil, message, subject, nil, 0x5, nil, nil, nil, assocGuid as NSString?, reaction!, range, summaryInfo as NSDictionary?)
                }
            }

            // DD scan and send
            if ddScan, let ddController = getSharedInstance("IMDDController") {
                let scanSel = NSSelectorFromString("scanMessage:outgoing:waitUntilDone:completionBlock:")
                if ddController.responds(to: scanSel) {
                    typealias ScanMethod = @convention(c) (NSObject, Selector, NSObject, Bool, Bool, @escaping @convention(block) (Int, Bool, Any?) -> Void) -> Void
                    let imp = ddController.method(for: scanSel)
                    let fn = unsafeBitCast(imp, to: ScanMethod.self)
                    fn(ddController, scanSel, messageToSend, true, true) { _, _, _ in
                        DispatchQueue.main.async {
                            safePerform(chat, selector: "sendMessage:", with: messageToSend)
                            if let transaction = transaction {
                                let lastGuid = safePerformReturning(chat, selector: "lastSentMessage")?.value(forKey: "guid") as? String ?? ""
                                IMHelper.respond(transaction: transaction, extra: ["identifier": lastGuid])
                            }
                        }
                    }
                    return
                }
            }

            // Non-DD-scan send
            safePerform(chat, selector: "sendMessage:", with: messageToSend)
            if let transaction = transaction {
                let lastGuid = safePerformReturning(chat, selector: "lastSentMessage")?.value(forKey: "guid") as? String ?? ""
                IMHelper.respond(transaction: transaction, extra: ["identifier": lastGuid])
            }
        }

        // Check if this is a reply or reaction (has selectedMessageGuid)
        if let selectedGuid = data["selectedMessageGuid"] as? String, !selectedGuid.isEmpty {
            getMessageItem(guid: selectedGuid) { message in
                guard let message = message else {
                    IMHelper.respondError(transaction: transaction, error: "Message not found: \(selectedGuid)")
                    return
                }

                let messageItem = safePerformReturning(message, selector: "_imMessageItem")
                let items = messageItem?.perform(NSSelectorFromString("_newChatItems"))?.takeUnretainedValue()
                let partIndex = data["partIndex"] as? Int ?? 0
                let item = items.flatMap { findPartChatItem(items: $0, partIndex: partIndex) }

                // Reaction
                if let reactionType = data["reactionType"] as? String, !reactionType.isEmpty {

                    // Emoji tapback (IMEmojiTapback + IMTapbackSender)
                    if let emoji = data["emoji"] as? String {
                        let isRemoved = reactionType.hasPrefix("-")
                        sendEmojiTapback(emoji: emoji, isRemoved: isRemoved, chat: chat, item: item, message: message, transaction: transaction)
                        return
                    }

                    // Sticker tapback (IMStickerTapback + IMTapbackSender)
                    if let stickerPath = data["stickerPath"] as? String {
                        let isRemoved = reactionType.hasPrefix("-")
                        sendStickerTapback(stickerPath: stickerPath, isRemoved: isRemoved, chat: chat, item: item, message: message, transaction: transaction)
                        return
                    }

                    // Classic tapback (IMMessage constructor)
                    let reactionLong = parseReactionType(reactionType)
                    let verb = reactionToVerb(reactionType)

                    // Get text for summary
                    var textString: String?
                    if let item = item, let text = safePerformReturning(item, selector: "text") {
                        textString = (text as? NSAttributedString)?.string
                    }
                    if textString == nil {
                        textString = (message.value(forKey: "text") as? NSAttributedString)?.string
                    }

                    let isAttachment: Bool = {
                        guard let t = textString else { return true }
                        if let data = t.data(using: .nonLossyASCII),
                           let encoded = String(data: data, encoding: .utf8) {
                            return encoded == "\\ufffc" || encoded.isEmpty
                        }
                        return false
                    }()

                    let messageGuid = message.value(forKey: "guid") as? String ?? ""
                    let summaryText = isAttachment ? "an attachment" : "\u{201C}\(textString ?? "")\u{201D}"
                    let newAttrStr = NSMutableAttributedString(string: verb + summaryText)

                    var assocGuid: String
                    var messageSummary: [String: Any]
                    var range: NSRange

                    if let item = item {
                        let itemText = safePerformReturning(item, selector: "text") as? NSAttributedString
                        messageSummary = isAttachment ? [:] : ["amc": 1, "ams": itemText?.string ?? textString ?? ""]

                        // Get message part range
                        let partRangeSel = NSSelectorFromString("messagePartRange")
                        if item.responds(to: partRangeSel) {
                            typealias RangeMethod = @convention(c) (NSObject, Selector) -> NSRange
                            let imp = item.method(for: partRangeSel)
                            let fn = unsafeBitCast(imp, to: RangeMethod.self)
                            range = fn(item, partRangeSel)
                        } else {
                            range = NSRange(location: 0, length: 0)
                        }

                        if isAttachment {
                            assocGuid = "p:\(partIndex)/\(messageGuid)"
                        } else if itemText == nil {
                            assocGuid = "bp:\(messageGuid)"
                        } else {
                            assocGuid = "p:\(partIndex)/\(messageGuid)"
                        }
                    } else {
                        messageSummary = isAttachment ? [:] : ["amc": 1, "ams": textString ?? ""]
                        range = NSRange(location: 0, length: textString?.count ?? 0)
                        assocGuid = messageGuid
                    }

                    createAndSend(newAttrStr, subjectStr, effectId, nil, assocGuid, reactionLong, range, messageSummary, nil, false, ddScan)
                } else {
                    // Thread reply
                    var threadId = message.value(forKey: "threadIdentifier") as? String ?? ""
                    if threadId.isEmpty, let item = item, let fn = resolved_IMCreateThreadIdentifier {
                        threadId = fn(item).takeUnretainedValue() as String
                    }
                    createAndSend(attrStr, subjectStr, effectId, threadId, nil, nil, NSRange(location: 0, length: 0), nil, transfers, isAudioMessage, ddScan)
                }
            }
        } else {
            // Simple message (no reply, no reaction)
            createAndSend(attrStr, subjectStr, effectId, nil, nil, nil, NSRange(location: 0, length: 0), nil, transfers, isAudioMessage, ddScan)
        }
    }

    // MARK: - Edit Message

    static func editMessage(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let messageGuid = data["messageGuid"] as? String else { return }

        getMessageItem(guid: messageGuid) { message in
            guard let message = message else {
                IMHelper.respondError(transaction: transaction, error: "Message not found for edit!")
                return
            }

            let editedText = data["editedMessage"] as? String ?? ""
            let bcText = data["backwardsCompatibilityMessage"] as? String ?? ""
            let partIndex = data["partIndex"] as? Int ?? 0

            let editedString = NSMutableAttributedString(string: editedText)
            let bcString = NSMutableAttributedString(string: bcText)

            guard let messageItem = safePerformReturning(message, selector: "_imMessageItem") else {
                IMHelper.respondError(transaction: transaction, error: "Failed to get message item for edit!")
                return
            }

            // Try Tahoe 5-arg selector first
            let tahoeSel = NSSelectorFromString("editMessageItem:atPartIndex:withNewPartText:newPartTranslation:backwardCompatabilityText:")
            let sequoiaSel = NSSelectorFromString("editMessageItem:atPartIndex:withNewPartText:backwardCompatabilityText:")

            if chat.responds(to: tahoeSel) {
                typealias EditMethod = @convention(c) (NSObject, Selector, NSObject, Int, NSMutableAttributedString, Any?, NSMutableAttributedString) -> Void
                let imp = chat.method(for: tahoeSel)
                let fn = unsafeBitCast(imp, to: EditMethod.self)
                fn(chat, tahoeSel, messageItem, partIndex, editedString, nil, bcString)
            } else if chat.responds(to: sequoiaSel) {
                typealias EditMethod = @convention(c) (NSObject, Selector, NSObject, Int, NSMutableAttributedString, NSMutableAttributedString) -> Void
                let imp = chat.method(for: sequoiaSel)
                let fn = unsafeBitCast(imp, to: EditMethod.self)
                fn(chat, sequoiaSel, messageItem, partIndex, editedString, bcString)
            } else {
                IMHelper.respondError(transaction: transaction, error: "No edit selector found!")
                return
            }

            IMHelper.respond(transaction: transaction)
        }
    }

    // MARK: - Unsend Message

    static func unsendMessage(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let messageGuid = data["messageGuid"] as? String else { return }
        let partIndex = data["partIndex"] as? Int ?? 0

        getMessageItem(guid: messageGuid) { message in
            guard let message = message,
                  let messageItem = safePerformReturning(message, selector: "_imMessageItem"),
                  let items = messageItem.perform(NSSelectorFromString("_newChatItems"))?.takeUnretainedValue() else {
                IMHelper.respondError(transaction: transaction, error: "Message not found for unsend!")
                return
            }

            if let item = findPartChatItem(items: items, partIndex: partIndex) {
                safePerform(chat, selector: "retractMessagePart:", with: item)
            }

            IMHelper.respond(transaction: transaction)
        }
    }

    // MARK: - Send Multipart

    static func sendMultipart(data: [String: Any], transaction: String?) {
        guard let parts = data["parts"] as? [[String: Any]] else { return }

        let attributedString = NSMutableAttributedString(string: "")
        var transfers: [String] = []

        for dict in parts {
            let partIndex = dict["partIndex"] as? Int ?? 0

            if let filePath = dict["filePath"] as? String, !filePath.isEmpty {
                // File attachment part
                let fileUrl = URL(fileURLWithPath: filePath)
                guard let transfer = AttachmentActions.prepareFileTransfer(url: fileUrl, filename: fileUrl.lastPathComponent) else { continue }
                let transferGuid = transfer.value(forKey: "guid") as? String ?? ""
                transfers.append(transferGuid)

                let attachStr = NSMutableAttributedString(string: "\u{FFFC}")
                attachStr.addAttributes([
                    NSAttributedString.Key(rawValue: "__kIMBaseWritingDirectionAttributeName"): "-1",
                    NSAttributedString.Key(rawValue: "__kIMFileTransferGUIDAttributeName"): transferGuid,
                    NSAttributedString.Key(rawValue: "__kIMFilenameAttributeName"): fileUrl.lastPathComponent,
                    NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"): NSNumber(value: partIndex),
                ], range: NSRange(location: 0, length: 1))
                attributedString.append(attachStr)
            } else {
                let text = dict["text"] as? String ?? ""
                if let mention = dict["mention"] as? String, !mention.isEmpty {
                    // Mention part
                    let mentionStr = NSMutableAttributedString(string: text)
                    mentionStr.addAttributes([
                        NSAttributedString.Key(rawValue: "__kIMBaseWritingDirectionAttributeName"): "-1",
                        NSAttributedString.Key(rawValue: "__kIMMentionConfirmedMention"): mention,
                        NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"): NSNumber(value: partIndex),
                    ], range: NSRange(location: 0, length: (text as NSString).length))
                    attributedString.append(mentionStr)
                } else {
                    // Plain text part
                    let msgStr = NSMutableAttributedString(string: text)
                    msgStr.addAttributes([
                        NSAttributedString.Key(rawValue: "__kIMBaseWritingDirectionAttributeName"): "-1",
                        NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"): NSNumber(value: partIndex),
                    ], range: NSRange(location: 0, length: (text as NSString).length))
                    attributedString.append(msgStr)
                }
            }
        }

        sendMessage(data: data, transfers: transfers, attributedString: attributedString, transaction: transaction)
    }

    // MARK: - Delete Message

    static func deleteMessage(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let messageGuid = data["messageGuid"] as? String else { return }

        getMessageItem(guid: messageGuid) { message in
            guard let message = message,
                  let messageItem = safePerformReturning(message, selector: "_imMessageItem"),
                  let items = messageItem.perform(NSSelectorFromString("_newChatItems"))?.takeUnretainedValue() else {
                IMHelper.respondError(transaction: transaction, error: "Message not found for delete!")
                return
            }

            if let arr = items as? [Any] {
                safePerform(chat, selector: "deleteChatItems:", with: arr as NSArray)
            } else {
                safePerform(chat, selector: "deleteChatItems:", with: [items] as NSArray)
            }

            IMHelper.respond(transaction: transaction)
        }
    }

    // MARK: - Notify Anyways

    static func notifyAnyways(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let messageGuid = data["messageGuid"] as? String else { return }

        getMessageItem(guid: messageGuid) { message in
            guard let message = message,
                  let messageItem = safePerformReturning(message, selector: "_imMessageItem"),
                  let items = messageItem.perform(NSSelectorFromString("_newChatItems"))?.takeUnretainedValue() else { return }

            let item: NSObject?
            if let arr = items as? [NSObject] {
                item = arr.first
            } else {
                item = items as? NSObject
            }

            if let item = item {
                safePerform(chat, selector: "markChatItemAsNotifyRecipient:", with: item)
                IMHelper.respond(transaction: transaction)
            }
        }
    }

    // MARK: - Search Messages

    static func searchMessages(data: [String: Any], transaction: String?) {
        guard let query = data["query"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide a search query!")
            return
        }
        let matchType = data["matchType"] as? String ?? "tokenized"

        // c=case-insensitive, d=diacritics, w=word-boundary, t=tokenized
        let suffix = matchType == "exact" ? "cwd" : "cwdt"
        let queryString = "kMDItemTextContent=\"\(query)\"\(suffix)"

        let queryContext = CSSearchQueryContext()
        queryContext.fetchAttributes = []
        let searchQuery = CSSearchQuery(queryString: queryString, queryContext: queryContext)

        // Use a lock to protect against concurrent foundItemsHandler calls
        let lock = NSLock()
        var results: [String] = []

        searchQuery.foundItemsHandler = { items in
            lock.lock()
            for item in items {
                results.append(item.uniqueIdentifier)
            }
            lock.unlock()
        }

        searchQuery.completionHandler = { error in
            if let error = error {
                IMHelper.respondError(transaction: transaction, error: error.localizedDescription)
            } else {
                lock.lock()
                let finalResults = results
                lock.unlock()
                IMHelper.respond(transaction: transaction, extra: ["results": finalResults])
            }
        }

        searchQuery.start()
    }

    // MARK: - Text Formatting

    /// Apply bold/italic/underline/strikethrough formatting ranges to an attributed string.
    static func applyTextFormatting(_ formatting: [[String: Any]]?, toMessage message: String) -> NSMutableAttributedString {
        let attrStr = NSMutableAttributedString(string: message)
        let messageLength = (message as NSString).length

        guard messageLength > 0,
              let formatting = formatting,
              !formatting.isEmpty else {
            return attrStr
        }

        // Always include message part attribute
        attrStr.addAttribute(NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"),
                             value: NSNumber(value: 0),
                             range: NSRange(location: 0, length: messageLength))

        for rangeDict in formatting {
            guard let start = rangeDict["start"] as? Int,
                  let length = rangeDict["length"] as? Int,
                  let styles = rangeDict["styles"] as? [String],
                  start >= 0, length > 0,
                  start + length <= messageLength else { continue }

            let range = NSRange(location: start, length: length)

            if styles.contains("bold") {
                attrStr.addAttribute(NSAttributedString.Key(rawValue: "__kIMTextBoldAttributeName"),
                                     value: NSNumber(value: 1), range: range)
            }
            if styles.contains("italic") {
                attrStr.addAttribute(NSAttributedString.Key(rawValue: "__kIMTextItalicAttributeName"),
                                     value: NSNumber(value: 1), range: range)
            }
            if styles.contains("underline") {
                attrStr.addAttribute(NSAttributedString.Key(rawValue: "__kIMTextUnderlineAttributeName"),
                                     value: NSNumber(value: 1), range: range)
            }
            if styles.contains("strikethrough") {
                attrStr.addAttribute(NSAttributedString.Key(rawValue: "__kIMTextStrikethroughAttributeName"),
                                     value: NSNumber(value: 1), range: range)
            }
        }

        return attrStr
    }
}
