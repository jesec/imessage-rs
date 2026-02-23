import XCTest
@testable import IMHelper

final class TextFormattingTests: XCTestCase {

    func testNoFormatting() {
        let result = MessageActions.applyTextFormatting(nil, toMessage: "Hello")
        XCTAssertEqual(result.string, "Hello")
        // No formatting attributes should be present (besides what we add)
    }

    func testEmptyFormatting() {
        let result = MessageActions.applyTextFormatting([], toMessage: "Hello")
        XCTAssertEqual(result.string, "Hello")
    }

    func testBoldFormatting() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 5, "styles": ["bold"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello World")
        XCTAssertEqual(result.string, "Hello World")

        // Check bold attribute exists on first 5 chars
        var effectiveRange = NSRange()
        let value = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextBoldAttributeName"),
                                     at: 0, effectiveRange: &effectiveRange)
        XCTAssertNotNil(value)
        XCTAssertEqual(effectiveRange.location, 0)
        XCTAssertEqual(effectiveRange.length, 5)
    }

    func testItalicFormatting() {
        let formatting: [[String: Any]] = [
            ["start": 6, "length": 5, "styles": ["italic"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello World")

        let value = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextItalicAttributeName"),
                                     at: 6, effectiveRange: nil)
        XCTAssertNotNil(value)
    }

    func testUnderlineFormatting() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 3, "styles": ["underline"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hey there")

        let value = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextUnderlineAttributeName"),
                                     at: 0, effectiveRange: nil)
        XCTAssertNotNil(value)
    }

    func testStrikethroughFormatting() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 4, "styles": ["strikethrough"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "done task")

        let value = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextStrikethroughAttributeName"),
                                     at: 0, effectiveRange: nil)
        XCTAssertNotNil(value)
    }

    func testMultipleStyles() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 5, "styles": ["bold", "italic"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello")

        let bold = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextBoldAttributeName"),
                                    at: 0, effectiveRange: nil)
        let italic = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextItalicAttributeName"),
                                      at: 0, effectiveRange: nil)
        XCTAssertNotNil(bold)
        XCTAssertNotNil(italic)
    }

    func testMultipleRanges() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 5, "styles": ["bold"]],
            ["start": 6, "length": 5, "styles": ["italic"]],
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello World")

        let bold = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextBoldAttributeName"),
                                    at: 0, effectiveRange: nil)
        let italic = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextItalicAttributeName"),
                                      at: 6, effectiveRange: nil)
        XCTAssertNotNil(bold)
        XCTAssertNotNil(italic)
    }

    func testOutOfBoundsIgnored() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 100, "styles": ["bold"]]  // Exceeds message length
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hi")
        XCTAssertEqual(result.string, "Hi")
        // Should not crash, the formatting should be ignored
        let value = result.attribute(NSAttributedString.Key(rawValue: "__kIMTextBoldAttributeName"),
                                     at: 0, effectiveRange: nil)
        XCTAssertNil(value)
    }

    func testNegativeRangeIgnored() {
        let formatting: [[String: Any]] = [
            ["start": -1, "length": 5, "styles": ["bold"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello")
        // Should not crash
        XCTAssertEqual(result.string, "Hello")
    }

    func testEmptyMessage() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 1, "styles": ["bold"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "")
        XCTAssertEqual(result.string, "")
    }

    func testMessagePartAttribute() {
        let formatting: [[String: Any]] = [
            ["start": 0, "length": 5, "styles": ["bold"]]
        ]
        let result = MessageActions.applyTextFormatting(formatting, toMessage: "Hello")

        // Check __kIMMessagePartAttributeName is set across entire string
        let partAttr = result.attribute(NSAttributedString.Key(rawValue: "__kIMMessagePartAttributeName"),
                                        at: 0, effectiveRange: nil)
        XCTAssertNotNil(partAttr)
        XCTAssertEqual(partAttr as? Int, 0)
    }
}
