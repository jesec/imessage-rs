import Foundation
import Network

/// TCP client using Network.framework (NWConnection).
/// All callbacks run on the main queue, matching the Obj-C GCDAsyncSocket behavior.
/// IMCore APIs must be called from the main thread.
final class TCPClient {
    private var connection: NWConnection?
    private let host: NWEndpoint.Host = .ipv4(.loopback)
    private let port: NWEndpoint.Port

    /// Called when a complete JSON message line is received (on main queue).
    var onMessage: ((String) -> Void)?

    /// Called when the TCP connection is established (on main queue).
    var onConnect: (() -> Void)?

    /// Buffer for accumulating partial reads until we hit \n.
    private var buffer = Data()

    init() {
        let uid = getuid()
        let computed = Int(45670) + Int(uid) - 501
        let clamped = min(max(computed, 45670), 65535)
        self.port = NWEndpoint.Port(rawValue: UInt16(clamped))!
        Log.info("TCPClient: will connect to port \(clamped)")
    }

    func connect() {
        let conn = NWConnection(host: host, port: port, using: .tcp)
        self.connection = conn

        conn.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                Log.info("TCPClient: connected to \(self.host):\(self.port)")
                self.buffer = Data()
                self.onConnect?()
                self.startReading()
            case .failed(let error):
                Log.error("TCPClient: connection failed: \(error)")
                self.scheduleReconnect()
            case .cancelled:
                Log.info("TCPClient: connection cancelled")
            case .waiting(let error):
                Log.info("TCPClient: waiting: \(error)")
            default:
                break
            }
        }

        conn.start(queue: .main)
    }

    func disconnect() {
        connection?.cancel()
        connection = nil
    }

    /// Send a dictionary as JSON + \r\n over the TCP socket.
    func send(_ dict: [String: Any]) {
        guard let conn = connection else {
            Log.error("TCPClient: cannot send, not connected")
            return
        }

        do {
            let jsonData = try JSONSerialization.data(withJSONObject: dict, options: [])
            guard var message = String(data: jsonData, encoding: .utf8) else { return }
            message += "\r\n"
            guard let data = message.data(using: .utf8) else { return }

            conn.send(content: data, completion: .contentProcessed { error in
                if let error = error {
                    Log.error("TCPClient: send error: \(error)")
                }
            })
        } catch {
            Log.error("TCPClient: JSON serialization error: \(error)")
        }
    }

    // MARK: - Private

    private func startReading() {
        guard let conn = connection else { return }

        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }

            if let data = data, !data.isEmpty {
                self.buffer.append(data)
                self.processBuffer()
            }

            if isComplete {
                Log.info("TCPClient: connection closed by server")
                self.scheduleReconnect()
                return
            }

            if let error = error {
                Log.error("TCPClient: read error: \(error)")
                self.scheduleReconnect()
                return
            }

            // Continue reading
            self.startReading()
        }
    }

    /// Extract complete lines delimited by \n (with optional \r before it) from the buffer.
    private func processBuffer() {
        let lf = UInt8(0x0A) // \n

        while let idx = buffer.firstIndex(of: lf) {
            var endOfLine = idx
            // Strip optional \r before \n
            if endOfLine > buffer.startIndex && buffer[buffer.index(before: endOfLine)] == 0x0D {
                endOfLine = buffer.index(before: endOfLine)
            }
            let lineData = buffer.subdata(in: buffer.startIndex..<endOfLine)
            buffer.removeSubrange(buffer.startIndex...idx)

            if let line = String(data: lineData, encoding: .utf8), !line.isEmpty {
                onMessage?(line)
            }
        }
    }

    private func scheduleReconnect() {
        connection?.cancel()
        connection = nil
        Log.info("TCPClient: reconnecting in 5 seconds...")
        DispatchQueue.main.asyncAfter(deadline: .now() + 5.0) { [weak self] in
            self?.connect()
        }
    }
}
