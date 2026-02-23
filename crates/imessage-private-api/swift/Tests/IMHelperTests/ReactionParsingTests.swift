import XCTest
@testable import IMHelper

final class ReactionParsingTests: XCTestCase {

    // MARK: - parseReactionType

    func testPositiveReactions() {
        XCTAssertEqual(parseReactionType("love"), 2000)
        XCTAssertEqual(parseReactionType("like"), 2001)
        XCTAssertEqual(parseReactionType("dislike"), 2002)
        XCTAssertEqual(parseReactionType("laugh"), 2003)
        XCTAssertEqual(parseReactionType("emphasize"), 2004)
        XCTAssertEqual(parseReactionType("question"), 2005)
    }

    func testNegativeReactions() {
        XCTAssertEqual(parseReactionType("-love"), 3000)
        XCTAssertEqual(parseReactionType("-like"), 3001)
        XCTAssertEqual(parseReactionType("-dislike"), 3002)
        XCTAssertEqual(parseReactionType("-laugh"), 3003)
        XCTAssertEqual(parseReactionType("-emphasize"), 3004)
        XCTAssertEqual(parseReactionType("-question"), 3005)
    }

    func testCaseInsensitive() {
        XCTAssertEqual(parseReactionType("Love"), 2000)
        XCTAssertEqual(parseReactionType("LIKE"), 2001)
        XCTAssertEqual(parseReactionType("-Love"), 3000)
    }

    func testUnknownReaction() {
        XCTAssertEqual(parseReactionType("unknown"), 0)
        XCTAssertEqual(parseReactionType(""), 0)
    }

    // MARK: - reactionToVerb

    func testPositiveVerbs() {
        XCTAssertEqual(reactionToVerb("love"), "Loved ")
        XCTAssertEqual(reactionToVerb("like"), "Liked ")
        XCTAssertEqual(reactionToVerb("dislike"), "Disliked ")
        XCTAssertEqual(reactionToVerb("laugh"), "Laughed at ")
        XCTAssertEqual(reactionToVerb("emphasize"), "Emphasized ")
        XCTAssertEqual(reactionToVerb("question"), "Questioned ")
    }

    func testNegativeVerbs() {
        XCTAssertEqual(reactionToVerb("-love"), "Removed a heart from ")
        XCTAssertEqual(reactionToVerb("-like"), "Removed a like from ")
        XCTAssertEqual(reactionToVerb("-dislike"), "Removed a dislike from ")
        XCTAssertEqual(reactionToVerb("-laugh"), "Removed a laugh from ")
        XCTAssertEqual(reactionToVerb("-emphasize"), "Removed an exclamation from ")
        XCTAssertEqual(reactionToVerb("-question"), "Removed a question mark from ")
    }

    func testUnknownVerb() {
        XCTAssertEqual(reactionToVerb("unknown"), "")
    }
}
