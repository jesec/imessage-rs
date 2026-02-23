import Foundation

enum AttachmentActions {

    // MARK: - Send Attachment

    static func sendAttachment(data: [String: Any], transaction: String?) {
        guard let filePath = data["filePath"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide a file path!")
            return
        }

        let fileUrl = URL(fileURLWithPath: filePath)
        guard let fileTransfer = prepareFileTransfer(url: fileUrl, filename: fileUrl.lastPathComponent) else {
            IMHelper.respondError(transaction: transaction, error: "Failed to prepare file transfer!")
            return
        }

        let transferGuid = fileTransfer.value(forKey: "guid") as? String ?? ""

        let attachmentStr = NSMutableAttributedString(string: "\u{FFFC}")
        attachmentStr.addAttributes([
            NSAttributedString.Key(rawValue: "__kIMBaseWritingDirectionAttributeName"): "-1",
            NSAttributedString.Key(rawValue: "__kIMFileTransferGUIDAttributeName"): transferGuid,
            NSAttributedString.Key(rawValue: "__kIMFilenameAttributeName"): fileUrl.lastPathComponent,
            NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"): NSNumber(value: 0),
        ], range: NSRange(location: 0, length: 1))

        MessageActions.sendMessage(data: data, transfers: [transferGuid], attributedString: attachmentStr, transaction: transaction)
    }

    // MARK: - Update Group Photo

    static func updateGroupPhoto(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }

        if let filePath = data["filePath"] as? String, !filePath.isEmpty {
            let fileUrl = URL(fileURLWithPath: filePath)
            guard let fileTransfer = prepareFileTransfer(url: fileUrl, filename: fileUrl.lastPathComponent) else {
                IMHelper.respondError(transaction: transaction, error: "Failed to prepare file transfer!")
                return
            }
            let transferGuid = fileTransfer.value(forKey: "guid") as? String ?? ""
            safePerform(chat, selector: "sendGroupPhotoUpdate:", with: transferGuid)
        } else {
            // Remove group photo by passing nil
            let sel = NSSelectorFromString("sendGroupPhotoUpdate:")
            if chat.responds(to: sel) {
                typealias UpdateMethod = @convention(c) (NSObject, Selector, Any?) -> Void
                let imp = chat.method(for: sel)
                let fn = unsafeBitCast(imp, to: UpdateMethod.self)
                fn(chat, sel, nil)
            }
        }

        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Download Purged Attachment

    static func downloadPurgedAttachment(data: [String: Any], transaction: String?) {
        guard let attachmentGuid = data["attachmentGuid"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide an attachment GUID!")
            return
        }

        guard let transferCenter = getSharedInstance("IMFileTransferCenter"),
              let transfer = safePerformReturning(transferCenter, selector: "transferForGUID:", with: attachmentGuid) else {
            IMHelper.respondError(transaction: transaction, error: "Transfer not found!")
            return
        }

        // Check transfer state: state != 0 or not incoming => no need to unpurge
        let state = callInt(transfer, selector: "transferState")
        let isIncoming = callBool(transfer, selector: "isIncoming")

        if state != 0 || !isIncoming {
            IMHelper.respondError(transaction: transaction, error: "No need to unpurge!")
            return
        }

        let guid = transfer.value(forKey: "guid") as? String ?? attachmentGuid
        safePerform(transferCenter, selector: "registerTransferWithDaemon:", with: guid)
        safePerform(transferCenter, selector: "acceptTransfer:", with: guid)
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Balloon Bundle Media Path

    static func balloonBundleMediaPath(data: [String: Any], transaction: String?) {
        guard getChat(guid: data["chatGuid"] as? String, transaction: transaction) != nil else { return }
        guard let messageGuid = data["messageGuid"] as? String else { return }

        getMessageItem(guid: messageGuid) { message in
            guard let message = message,
                  let messageItem = safePerformReturning(message, selector: "_imMessageItem"),
                  let items = messageItem.perform(NSSelectorFromString("_newChatItems"))?.takeUnretainedValue() else { return }

            let pluginClass: AnyClass? = NSClassFromString("IMTranscriptPluginChatItem")
            guard let pluginClass = pluginClass, (items as AnyObject).isKind(of: pluginClass) else { return }

            let item = items as! NSObject
            guard let dataSource = safePerformReturning(item, selector: "dataSource") else { return }

            let digitalTouchClass: AnyClass? = NSClassFromString("ETiOSMacBalloonPluginDataSource")
            let handwrittenClass: AnyClass? = NSClassFromString("HWiOSMacBalloonDataSource")

            if let dtCls = digitalTouchClass, dataSource.isKind(of: dtCls) {
                // Digital touch — generate media and return asset URL
                let sel = NSSelectorFromString("generateMedia:")
                if dataSource.responds(to: sel) {
                    typealias GenMethod = @convention(c) (NSObject, Selector, @escaping @convention(block) () -> Void) -> Void
                    let imp = dataSource.method(for: sel)
                    let fn = unsafeBitCast(imp, to: GenMethod.self)
                    fn(dataSource, sel) {
                        if let assetUrl = safePerformReturning(dataSource, selector: "assetURL") as? URL {
                            IMHelper.respond(transaction: transaction, extra: ["path": assetUrl.absoluteString])
                        }
                    }
                }
            } else if let hwCls = handwrittenClass, dataSource.isKind(of: hwCls) {
                // Handwritten message — generate image
                let sizeSel = NSSelectorFromString("sizeThatFits:")
                let genSel = NSSelectorFromString("generateImageForSize:completionHandler:")

                if dataSource.responds(to: sizeSel) && dataSource.responds(to: genSel) {
                    typealias SizeMethod = @convention(c) (NSObject, Selector, CGSize) -> CGSize
                    let sizeImp = dataSource.method(for: sizeSel)
                    let sizeFn = unsafeBitCast(sizeImp, to: SizeMethod.self)
                    let size = sizeFn(dataSource, sizeSel, CGSize(width: 300, height: 300))

                    typealias GenImgMethod = @convention(c) (NSObject, Selector, CGSize, @escaping @convention(block) (AnyObject?) -> Void) -> Void
                    let genImp = dataSource.method(for: genSel)
                    let genFn = unsafeBitCast(genImp, to: GenImgMethod.self)
                    genFn(dataSource, genSel, size) { url in
                        if let url = url as? URL {
                            IMHelper.respond(transaction: transaction, extra: ["path": url.absoluteString])
                        }
                    }
                }
            }
        }
    }

    // MARK: - File Transfer Preparation

    /// Create a new IMFileTransfer, copy the file to the persistent attachment path, and register with the daemon.
    /// Matches the Obj-C `+prepareFileTransferForAttachment:filename:`.
    static func prepareFileTransfer(url: URL, filename: String) -> NSObject? {
        guard let transferCenter = getSharedInstance("IMFileTransferCenter") else {
            Log.error("IMFileTransferCenter not available")
            return nil
        }

        // Create initial transfer GUID
        guard let initGuid = safePerformReturning(transferCenter, selector: "guidForNewOutgoingTransferWithLocalURL:", with: url as NSURL) as? String else {
            Log.error("Failed to get transfer GUID")
            return nil
        }

        // Get the transfer object
        guard let transfer = safePerformReturning(transferCenter, selector: "transferForGUID:", with: initGuid) else {
            Log.error("Failed to get transfer for GUID")
            return nil
        }

        // Get persistent path
        guard let attachController = getSharedInstance("IMDPersistentAttachmentController") else {
            Log.error("IMDPersistentAttachmentController not available")
            return nil
        }

        let pathSel = NSSelectorFromString("_persistentPathForTransfer:filename:highQuality:chatGUID:storeAtExternalPath:")
        guard attachController.responds(to: pathSel) else {
            Log.error("_persistentPathForTransfer selector not found")
            return nil
        }

        typealias PathMethod = @convention(c) (NSObject, Selector, NSObject, NSString, Bool, NSString?, Bool) -> NSString?
        let imp = attachController.method(for: pathSel)
        let fn = unsafeBitCast(imp, to: PathMethod.self)
        let persistentPath = fn(attachController, pathSel, transfer, filename as NSString, true, nil, true) as String?

        let guid = transfer.value(forKey: "guid") as? String ?? initGuid

        // persistentPath may be nil (e.g. on Tahoe for group photos) — Obj-C original
        // skips the copy/retarget in that case and still registers the transfer at its original path
        if let persistentPath = persistentPath {
            let persistentURL = URL(fileURLWithPath: persistentPath)

            // Create directory and copy file
            do {
                try FileManager.default.createDirectory(at: persistentURL.deletingLastPathComponent(),
                                                         withIntermediateDirectories: true)
                try FileManager.default.copyItem(at: url, to: persistentURL)
            } catch {
                Log.error("File preparation error: \(error)")
                return nil
            }

            // Retarget transfer to persistent path
            let retargetSel = NSSelectorFromString("retargetTransfer:toPath:")
            if transferCenter.responds(to: retargetSel) {
                typealias RetargetMethod = @convention(c) (NSObject, Selector, NSString, NSString) -> Void
                let retargetImp = transferCenter.method(for: retargetSel)
                let retargetFn = unsafeBitCast(retargetImp, to: RetargetMethod.self)
                retargetFn(transferCenter, retargetSel, guid as NSString, persistentPath as NSString)
            }

            // Update local URL
            transfer.setValue(persistentURL, forKey: "localURL")
        }

        // Register with daemon (file must be in correct location before this)
        safePerform(transferCenter, selector: "registerTransferWithDaemon:", with: guid)

        return transfer
    }
}
