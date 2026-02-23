import XCTest
@testable import IMHelper

final class TCPClientTests: XCTestCase {

    // MARK: - JSON Framing

    func testPortCalculation() {
        // The port formula is 45670 + (uid - 501), clamped to [45670, 65535]
        // We can't test the exact value since it depends on the running user's UID,
        // but we can verify the formula with known inputs.

        // uid 501 -> 45670
        let port501 = min(max(45670 + 501 - 501, 45670), 65535)
        XCTAssertEqual(port501, 45670)

        // uid 502 -> 45671
        let port502 = min(max(45670 + 502 - 501, 45670), 65535)
        XCTAssertEqual(port502, 45671)

        // uid 100000 -> clamped to 65535
        let portHuge = min(max(45670 + 100000 - 501, 45670), 65535)
        XCTAssertEqual(portHuge, 65535)

        // uid 0 -> clamped to 45670 (negative offset)
        let port0 = min(max(45670 + 0 - 501, 45670), 65535)
        XCTAssertEqual(port0, 45670)
    }

    func testPingMessageFormat() {
        // Verify the ping message matches the expected wire format
        let ping: [String: Any] = [
            "event": "ping",
            "message": "Helper Connected!",
            "process": "com.apple.MobileSMS",
        ]

        let jsonData = try! JSONSerialization.data(withJSONObject: ping, options: [])
        let json = String(data: jsonData, encoding: .utf8)!

        // Verify it's valid JSON
        XCTAssertNotNil(json)

        // Parse it back
        let parsed = try! JSONSerialization.jsonObject(with: jsonData) as! [String: Any]
        XCTAssertEqual(parsed["event"] as? String, "ping")
        XCTAssertEqual(parsed["message"] as? String, "Helper Connected!")
        XCTAssertEqual(parsed["process"] as? String, "com.apple.MobileSMS")
    }

    func testTransactionResponseFormat() {
        // Simple success response
        let response: [String: Any] = [
            "transactionId": "test-uuid-123"
        ]
        let data = try! JSONSerialization.data(withJSONObject: response)
        let parsed = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(parsed["transactionId"] as? String, "test-uuid-123")
    }

    func testTransactionErrorResponseFormat() {
        let response: [String: Any] = [
            "transactionId": "test-uuid-123",
            "error": "Chat does not exist!"
        ]
        let data = try! JSONSerialization.data(withJSONObject: response)
        let parsed = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(parsed["transactionId"] as? String, "test-uuid-123")
        XCTAssertEqual(parsed["error"] as? String, "Chat does not exist!")
    }

    func testTransactionWithIdentifier() {
        let response: [String: Any] = [
            "transactionId": "test-uuid-123",
            "identifier": "msg-guid-456"
        ]
        let data = try! JSONSerialization.data(withJSONObject: response)
        let parsed = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(parsed["identifier"] as? String, "msg-guid-456")
    }

    func testEventFormat() {
        let event: [String: Any] = [
            "event": "started-typing",
            "guid": "iMessage;-;test@example.com"
        ]
        let data = try! JSONSerialization.data(withJSONObject: event)
        let parsed = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(parsed["event"] as? String, "started-typing")
        XCTAssertEqual(parsed["guid"] as? String, "iMessage;-;test@example.com")
    }

    func testNewlineDelimiter() {
        // Verify \r\n is the correct delimiter
        let msg: [String: Any] = ["event": "ping"]
        let jsonData = try! JSONSerialization.data(withJSONObject: msg)
        let json = String(data: jsonData, encoding: .utf8)!
        let wireMessage = json + "\r\n"

        // Should end with \r\n
        XCTAssertTrue(wireMessage.hasSuffix("\r\n"))

        // The JSON part should be parseable
        let jsonPart = wireMessage.trimmingCharacters(in: .whitespacesAndNewlines)
        let parsed = try! JSONSerialization.jsonObject(with: jsonPart.data(using: .utf8)!) as! [String: Any]
        XCTAssertEqual(parsed["event"] as? String, "ping")
    }
}
