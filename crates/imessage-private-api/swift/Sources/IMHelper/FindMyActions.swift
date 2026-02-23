import Foundation
import CoreLocation

enum FindMyActions {

    // MARK: - Handle ID Extraction

    /// Extract a handle identifier string using multiple strategies.
    /// Works across IMFindMyHandle (Tahoe), IMHandle (Sequoia), and FMLHandle.
    static func extractHandleId(_ handleObj: AnyObject, handleIdMap: [String: String] = [:]) -> String? {
        guard let obj = handleObj as? NSObject else { return nil }

        // Strategy 1: .identifier (IMFindMyHandle, some IMHandle)
        if obj.responds(to: NSSelectorFromString("identifier")),
           let ident = safePerformReturning(obj, selector: "identifier") as? String {
            return ident
        }

        // Strategy 2: .ID (IMHandle)
        let idSel = NSSelectorFromString("ID")
        if obj.responds(to: idSel),
           let ident = obj.perform(idSel)?.takeUnretainedValue() as? String {
            return ident
        }

        // Strategy 3: Lookup map (FMLHandle description → identifier)
        let desc = obj.description
        if let mapped = handleIdMap[desc] {
            return mapped
        }

        // Strategy 4: Parse description "Handle:+15042878167 Handle Type:1..."
        if let range = desc.range(of: "Handle:") {
            let rest = String(desc[range.upperBound...])
            if let spaceRange = rest.range(of: " ") {
                return String(rest[rest.startIndex..<spaceRange.lowerBound])
            }
            return rest
        }

        return nil
    }

    // MARK: - Get FindMy Decryption Key

    /// Read the FMIPDataManager symmetric key from the macOS Keychain.
    /// Only works inside FindMy.app (requires com.apple.private.security.storage.FindMy entitlement).
    /// Returns the 32-byte key as base64 in the transaction response.
    static func getFindMyKey(data: [String: Any], transaction: String?) {
        let keyQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "FMIPDataManager",
            kSecMatchLimit as String: kSecMatchLimitOne,
            kSecReturnData as String: true,
        ]

        var keyItem: CFTypeRef?
        let keyStatus = SecItemCopyMatching(keyQuery as CFDictionary, &keyItem)

        guard keyStatus == errSecSuccess, let keyData = keyItem as? Data else {
            let desc = SecCopyErrorMessageString(keyStatus, nil) as String? ?? "unknown"
            IMHelper.respondError(transaction: transaction,
                error: "Failed to read FMIPDataManager from Keychain (status \(keyStatus): \(desc))")
            return
        }

        // Parse the keychain data as bplist to extract symmetricKey
        guard let keyPlist = try? PropertyListSerialization.propertyList(from: keyData, options: [], format: nil) as? [String: Any] else {
            IMHelper.respondError(transaction: transaction, error: "Failed to parse FMIPDataManager keychain data as plist")
            return
        }

        let symmetricKeyBytes: Data
        if let skData = keyPlist["symmetricKey"] as? Data {
            symmetricKeyBytes = skData
        } else if let skDict = keyPlist["symmetricKey"] as? [String: Any],
                  let innerKey = skDict["key"] as? [String: Any],
                  let innerData = innerKey["data"] as? Data {
            symmetricKeyBytes = innerData
        } else {
            IMHelper.respondError(transaction: transaction, error: "symmetricKey not found in FMIPDataManager plist")
            return
        }

        guard symmetricKeyBytes.count == 32 else {
            IMHelper.respondError(transaction: transaction, error: "symmetricKey is \(symmetricKeyBytes.count) bytes, expected 32")
            return
        }

        let b64 = symmetricKeyBytes.base64EncodedString()
        IMHelper.respond(transaction: transaction, extra: ["key": b64])
    }

    // MARK: - Refresh FindMy Friends

    static func refreshFindMyFriends(data: [String: Any], transaction: String?) {
        guard let imfmfSession = getSharedInstance("IMFMFSession"),
              let fmlSession = safePerformReturning(imfmfSession, selector: "fmlSession") else {
            IMHelper.respondError(transaction: transaction, error: "FindMy session not available!")
            return
        }

        let getSel = NSSelectorFromString("getFriendsSharingLocationsWithMeWithCompletion:")
        guard fmlSession.responds(to: getSel) else {
            IMHelper.respondError(transaction: transaction, error: "getFriendsSharingLocationsWithMeWithCompletion: not found!")
            return
        }

        // Build a handle-id lookup map from IMFMFSession.findMyHandlesSharingLocationWithMe
        // This resolves FMLHandle objects (which lack .identifier on macOS 14-15) to their
        // identifier strings via the IMFindMyHandle wrapper.
        var handleIdMap = [String: String]()
        let handlesSel = NSSelectorFromString("findMyHandlesSharingLocationWithMe")
        if imfmfSession.responds(to: handlesSel),
           let handles = imfmfSession.perform(handlesSel)?.takeUnretainedValue() as? [NSObject] {
            for h in handles {
                if let ident = safePerformReturning(h, selector: "identifier") as? String {
                    // Map the FMLHandle's description to the identifier
                    if let fmlH = safePerformReturning(h, selector: "fmlHandle") {
                        handleIdMap[fmlH.description] = ident
                    }
                }
            }
        }

        typealias GetFriendsMethod = @convention(c) (NSObject, Selector, @escaping @convention(block) (NSArray?) -> Void) -> Void
        let imp = fmlSession.method(for: getSel)
        let fn = unsafeBitCast(imp, to: GetFriendsMethod.self)

        let capturedTransaction = transaction
        let capturedMap = handleIdMap

        fn(fmlSession, getSel) { friends in
            guard let friends = friends else {
                IMHelper.respond(transaction: capturedTransaction, extra: ["locations": [Any]()])
                return
            }

            // Extract handles from friends (version-adaptive)
            var allHandles = [NSObject]()
            for friend in friends {
                guard let friendObj = friend as? NSObject else { continue }
                // macOS 14-15: friendship objects have .handle returning FMLHandle
                if friendObj.responds(to: NSSelectorFromString("handle")),
                   let handle = friendObj.perform(NSSelectorFromString("handle"))?.takeUnretainedValue() as? NSObject {
                    allHandles.append(handle)
                    continue
                }
                // macOS 26+: friendObj might already be an FMLHandle or IMFindMyHandle
                if friendObj.responds(to: NSSelectorFromString("identifier")) {
                    allHandles.append(friendObj)
                    continue
                }
                // macOS 26+: try .fmlHandle (IMFindMyHandle wraps FMLHandle)
                if let fmlHandle = safePerformReturning(friendObj, selector: "fmlHandle") {
                    allHandles.append(fmlHandle)
                }
            }

            guard !allHandles.isEmpty else {
                IMHelper.respond(transaction: capturedTransaction, extra: ["locations": [Any]()])
                return
            }

            // Collect locations asynchronously via the locationUpdateCallback
            let locations = NSMutableArray()
            let locationsLock = NSLock()
            let totalHandles = allHandles.count
            let completedHandles = NSMutableSet()
            let completedLock = NSLock()
            let responded = NSMutableArray(array: [false])  // Box a bool
            let respondedLock = NSLock()

            let sendResponse = {
                respondedLock.lock()
                let alreadyResponded = responded[0] as? Bool ?? false
                if !alreadyResponded {
                    responded[0] = true
                    respondedLock.unlock()
                    locationsLock.lock()
                    let result = locations as [AnyObject]
                    locationsLock.unlock()
                    IMHelper.respond(transaction: capturedTransaction, extra: ["locations": result])
                } else {
                    respondedLock.unlock()
                }
            }

            // Save and restore original callback
            let origCallback = fmlSession.value(forKey: "locationUpdateCallback")

            // 15-second timeout: send whatever we have
            DispatchQueue.main.asyncAfter(deadline: .now() + 15.0) {
                fmlSession.setValue(origCallback, forKey: "locationUpdateCallback")
                sendResponse()
            }

            // Set callback to capture location updates
            let callback: @convention(block) (AnyObject, AnyObject) -> Void = { locationArg, handleArg in
                let handleId = extractHandleId(handleArg, handleIdMap: capturedMap)
                guard let handleId = handleId else { return }

                // Serialize the location
                if let locDetails = serializeLocationObject(locationArg as? NSObject, handleId: handleId) {
                    locationsLock.lock()
                    locations.add(locDetails)
                    locationsLock.unlock()
                }

                completedLock.lock()
                completedHandles.add(handleId)
                let completed = completedHandles.count
                completedLock.unlock()

                if completed >= totalHandles {
                    fmlSession.setValue(origCallback, forKey: "locationUpdateCallback")
                    sendResponse()
                }
            }
            fmlSession.setValue(callback, forKey: "locationUpdateCallback")

            // Trigger refresh for all handles
            let refreshSel = NSSelectorFromString("startRefreshingLocationForHandles:priority:isFromGroup:reverseGeocode:completion:")
            if fmlSession.responds(to: refreshSel) {
                typealias RefreshMethod = @convention(c) (NSObject, Selector, NSArray, Int, Bool, Bool, @escaping @convention(block) () -> Void) -> Void
                let rImp = fmlSession.method(for: refreshSel)
                let rFn = unsafeBitCast(rImp, to: RefreshMethod.self)
                rFn(fmlSession, refreshSel, allHandles as NSArray, 1000, false, true) {
                    // In completion, try cached locations for handles that haven't reported yet
                    let cacheSel = NSSelectorFromString("cachedLocationForHandle:includeAddress:")
                    guard fmlSession.responds(to: cacheSel) else { return }

                    for handle in allHandles {
                        let handleId = extractHandleId(handle, handleIdMap: capturedMap)
                        guard let handleId = handleId else { continue }

                        completedLock.lock()
                        let alreadyDone = completedHandles.contains(handleId)
                        completedLock.unlock()
                        if alreadyDone { continue }

                        typealias CacheMethod = @convention(c) (NSObject, Selector, NSObject, Bool) -> AnyObject?
                        let cImp = fmlSession.method(for: cacheSel)
                        let cFn = unsafeBitCast(cImp, to: CacheMethod.self)
                        if let cachedLoc = cFn(fmlSession, cacheSel, handle, true) as? NSObject {
                            if let locDetails = serializeLocationObject(cachedLoc, handleId: handleId) {
                                locationsLock.lock()
                                locations.add(locDetails)
                                locationsLock.unlock()

                                completedLock.lock()
                                completedHandles.add(handleId)
                                let completed = completedHandles.count
                                completedLock.unlock()

                                if completed >= totalHandles {
                                    fmlSession.setValue(origCallback, forKey: "locationUpdateCallback")
                                    sendResponse()
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // MARK: - Location Serialization

    /// Serialize a location object to a dictionary, detecting whether it's FMFLocation, FMLLocation,
    /// or IMFindMyLocation (Tahoe wrapper).
    static func serializeLocationObject(_ location: NSObject?, handleId: String) -> [String: Any]? {
        guard let location = location else { return nil }

        // macOS 26+: IMFindMyLocation wraps fmlLocation and/or fmfLocation
        if location.responds(to: NSSelectorFromString("fmlLocation")),
           let inner = safePerformReturning(location, selector: "fmlLocation") {
            return serializeFMLLocation(inner, handleId: handleId)
        }
        if location.responds(to: NSSelectorFromString("fmfLocation")),
           let inner = safePerformReturning(location, selector: "fmfLocation") {
            return serializeFMFLocation(inner, handleId: handleId)
        }

        // Direct FMLLocation (has .latitude/.longitude as doubles)
        let latSel = NSSelectorFromString("latitude")
        if location.responds(to: latSel) {
            return serializeFMLLocation(location, handleId: handleId)
        }

        // Direct FMFLocation (has .coordinate returning CLLocationCoordinate2D)
        let coordSel = NSSelectorFromString("coordinate")
        if location.responds(to: coordSel) {
            return serializeFMFLocation(location, handleId: handleId)
        }

        return nil
    }

    /// Serialize an FMFLocation object (CLLocationCoordinate2D-based) to a dictionary.
    static func serializeFMFLocation(_ location: NSObject, handleId: String? = nil) -> [String: Any] {
        // Get handle identifier (from the location object itself, or use provided)
        let resolvedHandleId: String?
        if let handleId = handleId {
            resolvedHandleId = handleId
        } else {
            let handle = safePerformReturning(location, selector: "handle")
            resolvedHandleId = handle.flatMap { safePerformReturning($0, selector: "identifier") as? String }
        }

        // Get coordinates via CLLocationCoordinate2D
        var latitude: Double = 0
        var longitude: Double = 0
        let coordSel = NSSelectorFromString("coordinate")
        if location.responds(to: coordSel) {
            typealias CoordMethod = @convention(c) (NSObject, Selector) -> CLLocationCoordinate2D
            let imp = location.method(for: coordSel)
            let fn = unsafeBitCast(imp, to: CoordMethod.self)
            let coord = fn(location, coordSel)
            latitude = coord.latitude
            longitude = coord.longitude
        }

        // Get other properties
        let longAddress = safePerformReturning(location, selector: "longAddress") as? String
        let shortAddress = safePerformReturning(location, selector: "shortAddress") as? String
        let subtitle = safePerformReturning(location, selector: "subtitle") as? String
        let title = safePerformReturning(location, selector: "title") as? String

        // Get timestamp
        var lastUpdated: Double = 0
        if let timestamp = safePerformReturning(location, selector: "timestamp") as? Date {
            lastUpdated = (timestamp.timeIntervalSince1970 * 1000).rounded()
        }

        // isLocatingInProgress
        let isLocating = callBool(location, selector: "isLocatingInProgress")

        // Location type
        let locationType = callInt(location, selector: "locationType")
        let status: String
        switch locationType {
        case 0: status = "legacy"
        case 2: status = "live"
        default: status = "shallow"
        }

        return [
            "handle": resolvedHandleId ?? NSNull(),
            "coordinates": [latitude, longitude],
            "long_address": longAddress ?? NSNull(),
            "short_address": shortAddress ?? NSNull(),
            "subtitle": subtitle ?? NSNull(),
            "title": title ?? NSNull(),
            "last_updated": lastUpdated,
            "is_locating_in_progress": isLocating ? 1 : 0,
            "status": status,
        ]
    }

    /// Serialize an FMLLocation object (double lat/lon properties) to a dictionary.
    /// FMLLocation uses .latitude/.longitude instead of .coordinate, .address (FMLPlaceMark)
    /// instead of .longAddress/.shortAddress, .coarseAddressLabel, and .labels array for title/subtitle.
    static func serializeFMLLocation(_ location: NSObject, handleId: String) -> [String: Any] {
        // Get coordinates as doubles
        var latitude: Double = 0
        var longitude: Double = 0
        let latSel = NSSelectorFromString("latitude")
        let lonSel = NSSelectorFromString("longitude")
        if location.responds(to: latSel) {
            typealias DoubleMethod = @convention(c) (NSObject, Selector) -> Double
            let latImp = location.method(for: latSel)
            let latFn = unsafeBitCast(latImp, to: DoubleMethod.self)
            latitude = latFn(location, latSel)
        }
        if location.responds(to: lonSel) {
            typealias DoubleMethod = @convention(c) (NSObject, Selector) -> Double
            let lonImp = location.method(for: lonSel)
            let lonFn = unsafeBitCast(lonImp, to: DoubleMethod.self)
            longitude = lonFn(location, lonSel)
        }

        // Address: .address returns FMLPlaceMark. Use its .description (convention),
        // but supplement with administrativeArea/subAdministrativeArea since .description omits them.
        var longAddress: String?
        var shortAddress: String?
        if let placemark = safePerformReturning(location, selector: "address") {
            var desc = placemark.description
            let extras = [
                "subAdministrativeArea",
                "administrativeArea",
            ]
            for selector in extras {
                if let val = safePerformReturning(placemark, selector: selector) as? String, !val.isEmpty {
                    desc += ", \(selector): \(val)"
                }
            }
            longAddress = desc
        }

        // Short address: prefer coarseAddressLabel, otherwise build "City, State" from placemark.
        if let coarse = safePerformReturning(location, selector: "coarseAddressLabel") {
            let str = (coarse as? String) ?? "\(coarse)"
            if !str.isEmpty { shortAddress = str }
        }
        if shortAddress == nil, let placemark = safePerformReturning(location, selector: "address") {
            shortAddress = buildShortAddress(placemark)
        }

        // Title and subtitle from .labels array (may be empty)
        var title: String?
        var subtitle: String?
        if let labels = safePerformReturning(location, selector: "labels") as? NSArray {
            if labels.count > 0, let s = labels[0] as? String, !s.isEmpty { title = s }
            if labels.count > 1, let s = labels[1] as? String, !s.isEmpty { subtitle = s }
        }

        // Timestamp
        var lastUpdated: Double = 0
        let tsSel = NSSelectorFromString("timestamp")
        if location.responds(to: tsSel) {
            typealias DoubleMethod = @convention(c) (NSObject, Selector) -> Double
            let tsImp = location.method(for: tsSel)
            let tsFn = unsafeBitCast(tsImp, to: DoubleMethod.self)
            let ts = tsFn(location, tsSel)
            lastUpdated = (ts * 1000).rounded()
        }

        // Location type
        let locationType = callInt(location, selector: "locationType")
        let status: String
        switch locationType {
        case 0: status = "legacy"
        case 2: status = "live"
        default: status = "shallow"
        }

        return [
            "handle": handleId,
            "coordinates": [latitude, longitude],
            "long_address": longAddress ?? NSNull(),
            "short_address": shortAddress ?? NSNull(),
            "subtitle": subtitle ?? NSNull(),
            "title": title ?? NSNull(),
            "last_updated": lastUpdated,
            "is_locating_in_progress": 0,
            "status": status,
        ]
    }

    /// Build a short "City, State" address from an FMLPlaceMark.
    /// Tries pairs in priority order, falling back until something non-empty is found.
    private static func buildShortAddress(_ placemark: NSObject) -> String? {
        func get(_ sel: String) -> String? {
            guard let val = safePerformReturning(placemark, selector: sel) as? String, !val.isEmpty else { return nil }
            return val
        }

        let locality = get("locality")
        let subAdminArea = get("subAdministrativeArea")
        let stateCode = get("stateCode")
        let adminArea = get("administrativeArea")
        let country = get("country")

        if let l = locality, let s = stateCode { return "\(l), \(s)" }
        if let l = locality, let a = adminArea { return "\(l), \(a)" }
        if let l = locality, let c = country { return "\(l), \(c)" }
        if let s = subAdminArea, let a = adminArea { return "\(s), \(a)" }
        if let s = subAdminArea, let c = country { return "\(s), \(c)" }
        if let a = adminArea, let c = country { return "\(a), \(c)" }
        // Single component fallbacks
        if let l = locality { return l }
        if let s = subAdminArea { return s }
        if let a = adminArea { return a }
        if let c = country { return c }

        return nil
    }

}
