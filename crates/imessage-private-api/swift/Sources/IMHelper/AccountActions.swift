import Foundation

enum AccountActions {

    // MARK: - Focus Status

    static func checkFocusStatus(data: [String: Any], transaction: String?) {
        guard let address = data["address"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide an address!")
            return
        }

        guard let handle = getIMHandle(address: address) else {
            IMHelper.respondError(transaction: transaction, error: "Handle not found!")
            return
        }

        guard let cls = NSClassFromString("IMHandleAvailabilityManager"),
              let manager = getSharedInstance("IMHandleAvailabilityManager") else {
            IMHelper.respondError(transaction: transaction, error: "IMHandleAvailabilityManager not available!")
            return
        }

        let fetchSel = NSSelectorFromString("_fetchUpdatedStatusForHandle:completion:")
        guard cls.instancesRespond(to: fetchSel) else {
            IMHelper.respondError(transaction: transaction, error: "Selector not found!")
            return
        }

        typealias FetchMethod = @convention(c) (NSObject, Selector, NSObject, @escaping @convention(block) () -> Void) -> Void
        let imp = manager.method(for: fetchSel)
        let fn = unsafeBitCast(imp, to: FetchMethod.self)
        fn(manager, fetchSel, handle) {
            // Delay 1 second to ensure latest status
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                let availSel = NSSelectorFromString("availabilityForHandle:")
                typealias AvailMethod = @convention(c) (NSObject, Selector, NSObject) -> Int
                let availImp = manager.method(for: availSel)
                let availFn = unsafeBitCast(availImp, to: AvailMethod.self)
                let status = availFn(manager, availSel, handle)
                let silenced = status == 2
                IMHelper.respond(transaction: transaction, extra: ["silenced": silenced])
            }
        }
    }

    // MARK: - Availability Check

    static func checkAvailability(data: [String: Any], transaction: String?, service: String) {
        guard let address = data["address"] as? String,
              let aliasType = data["aliasType"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide address and aliasType!")
            return
        }

        // Resolved symbols are Optional at the Swift level — the _Nullable globals
        // are bridged as ImplicitlyUnwrappedOptional, so we test via Optional binding.
        let svcName: String?
        if service == "iMessage" {
            svcName = resolved_IDSServiceNameiMessage
        } else {
            svcName = resolved_IDSServiceNameFaceTime
        }
        guard let serviceName = svcName else {
            IMHelper.respondError(transaction: transaction, error: "IDS service name not resolved!")
            return
        }

        let dest: AnyObject
        if aliasType == "phone" {
            guard let fn = resolved_IDSCopyIDForPhoneNumber else {
                IMHelper.respondError(transaction: transaction, error: "IDSCopyIDForPhoneNumber not resolved!")
                return
            }
            dest = fn(address as CFString) as AnyObject
        } else {
            guard let fn = resolved_IDSCopyIDForEmailAddress else {
                IMHelper.respondError(transaction: transaction, error: "IDSCopyIDForEmailAddress not resolved!")
                return
            }
            dest = fn(address as CFString) as AnyObject
        }

        guard let queryController = getSharedInstance("IDSIDQueryController") else {
            IMHelper.respondError(transaction: transaction, error: "IDSIDQueryController not available!")
            return
        }

        let sel = NSSelectorFromString("forceRefreshIDStatusForDestinations:service:listenerID:queue:completionBlock:")
        guard queryController.responds(to: sel) else {
            IMHelper.respondError(transaction: transaction, error: "forceRefreshIDStatusForDestinations selector not found!")
            return
        }

        let queue = DispatchQueue(label: "HandleIDS")

        typealias RefreshMethod = @convention(c) (NSObject, Selector, NSArray, AnyObject, NSString, DispatchQueue, @escaping @convention(block) (NSDictionary) -> Void) -> Void
        let imp = queryController.method(for: sel)
        let fn = unsafeBitCast(imp, to: RefreshMethod.self)
        fn(queryController, sel, [dest] as NSArray, serviceName as AnyObject, "SOIDSListener-com.apple.imessage-rest" as NSString, queue) { response in
            let status = (response.allValues.first as? NSNumber)?.intValue ?? 0
            let available = status == 1
            IMHelper.respond(transaction: transaction, extra: ["available": available])
        }
    }

    // MARK: - Account Info

    static func getAccountInfo(data: [String: Any], transaction: String?) {
        guard let accountController = getSharedInstance("IMAccountController") else {
            IMHelper.respondError(transaction: transaction, error: "IMAccountController not available!")
            return
        }

        guard let account = safePerformReturning(accountController, selector: "activeIMessageAccount") else {
            IMHelper.respondError(transaction: transaction, error: "No active iMessage account!")
            return
        }

        let smsAccount = safePerformReturning(accountController, selector: "activeSMSAccount")

        let appleId = safePerformReturning(account, selector: "strippedLogin") as? String
        let loginHandle = safePerformReturning(account, selector: "loginIMHandle")
        let accountName = loginHandle.flatMap { safePerformReturning($0, selector: "fullName") as? String }
        let smsEnabled = smsAccount.map { callBool($0, selector: "allowsSMSRelay") } ?? false
        let smsCapable = smsAccount.map { callBool($0, selector: "isSMSRelayCapable") } ?? false
        let statusMsg = safePerformReturning(account, selector: "loginStatusMessage") as? String
        let activeAlias = safePerformReturning(account, selector: "displayName") as? String

        IMHelper.respond(transaction: transaction, extra: [
            "apple_id": appleId as Any,
            "account_name": accountName as Any,
            "sms_forwarding_enabled": smsEnabled,
            "sms_forwarding_capable": smsCapable,
            "vetted_aliases": getAliases(vetted: true),
            "aliases": getAliases(vetted: false),
            "login_status_message": statusMsg as Any,
            "active_alias": activeAlias as Any,
        ])
    }

    // MARK: - Modify Active Alias

    static func modifyActiveAlias(data: [String: Any], transaction: String?) {
        guard let alias = data["alias"] as? String else {
            IMHelper.respondError(transaction: transaction, error: "Provide an alias!")
            return
        }

        guard isAccountEnabled() else {
            IMHelper.respondError(transaction: transaction, error: "Unable to modify alias")
            return
        }

        guard let accountController = getSharedInstance("IMAccountController"),
              let account = safePerformReturning(accountController, selector: "activeIMessageAccount") else {
            IMHelper.respondError(transaction: transaction, error: "No active iMessage account!")
            return
        }

        safePerform(account, selector: "setDisplayName:", with: alias)
        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Nickname Sharing

    static func shouldOfferNicknameSharing(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let nicknameController = getSharedInstance("IMNicknameController") else {
            IMHelper.respondError(transaction: transaction, error: "IMNicknameController not available!")
            return
        }

        let sel = NSSelectorFromString("shouldOfferNicknameSharingForChat:")
        if nicknameController.responds(to: sel) {
            typealias OfferMethod = @convention(c) (NSObject, Selector, NSObject) -> Bool
            let imp = nicknameController.method(for: sel)
            let fn = unsafeBitCast(imp, to: OfferMethod.self)
            let offer = fn(nicknameController, sel, chat)
            IMHelper.respond(transaction: transaction, extra: ["share": offer])
        }
    }

    static func shareNickname(data: [String: Any], transaction: String?) {
        guard let chat = getChat(guid: data["chatGuid"] as? String, transaction: transaction) else { return }
        guard let nicknameController = getSharedInstance("IMNicknameController") else { return }

        let participants = chat.value(forKey: "participants") as? [NSObject] ?? []

        let sel = NSSelectorFromString("allowHandlesForNicknameSharing:forChat:")
        if nicknameController.responds(to: sel) {
            typealias AllowMethod = @convention(c) (NSObject, Selector, NSArray, NSObject) -> Void
            let imp = nicknameController.method(for: sel)
            let fn = unsafeBitCast(imp, to: AllowMethod.self)
            fn(nicknameController, sel, participants as NSArray, chat)
        }

        IMHelper.respond(transaction: transaction)
    }

    // MARK: - Nickname Info

    static func getNicknameInfo(data: [String: Any], transaction: String?) {
        guard let nicknameController = getSharedInstance("IMNicknameController") else {
            IMHelper.respondError(transaction: transaction, error: "IMNicknameController not available!")
            return
        }

        let address = data["address"]
        var name: String?
        var avatarPath: String?

        if address is NSNull || address == nil {
            // Personal nickname
            if let nickname = safePerformReturning(nicknameController, selector: "personalNickname") {
                name = safePerformReturning(nickname, selector: "displayName") as? String
                if let avatar = safePerformReturning(nickname, selector: "avatar") {
                    avatarPath = safePerformReturning(avatar, selector: "imageFilePath") as? String
                }
            }
        } else if let addr = address as? String {
            // Per-handle nickname
            guard let handle = getIMHandle(address: addr) else {
                IMHelper.respondError(transaction: transaction, error: "Handle not found!")
                return
            }
            if let nickname = safePerformReturning(nicknameController, selector: "nicknameForHandle:", with: handle) {
                name = safePerformReturning(nickname, selector: "displayName") as? String
                if let avatar = safePerformReturning(nickname, selector: "avatar") {
                    avatarPath = safePerformReturning(avatar, selector: "imageFilePath") as? String
                }
            }
        }

        IMHelper.respond(transaction: transaction, extra: [
            "name": name ?? NSNull(),
            "avatar_path": avatarPath ?? NSNull(),
        ])
    }

    // MARK: - Helpers

    private static func isAccountEnabled() -> Bool {
        guard let accountController = getSharedInstance("IMAccountController"),
              let account = safePerformReturning(accountController, selector: "activeIMessageAccount") else {
            return false
        }
        return callBool(account, selector: "isActive") &&
               callBool(account, selector: "isRegistered") &&
               callBool(account, selector: "isOperational") &&
               callBool(account, selector: "isConnected")
    }

    private static func getAliases(vetted: Bool) -> [[String: Any]] {
        guard isAccountEnabled(),
              let accountController = getSharedInstance("IMAccountController"),
              let account = safePerformReturning(accountController, selector: "activeIMessageAccount") else {
            return []
        }

        let selectorName = vetted ? "vettedAliases" : "aliases"
        guard let aliases = safePerformReturning(account, selector: selectorName) as? [NSObject] else {
            return []
        }

        var result: [[String: Any]] = []
        for alias in aliases {
            let infoSel = NSSelectorFromString("_aliasInfoForAlias:")
            if account.responds(to: infoSel),
               let info = safePerformReturning(account, selector: "_aliasInfoForAlias:", with: alias) as? [String: Any] {
                result.append(info)
            } else {
                result.append(["Alias": alias])
            }
        }
        return result
    }
}
