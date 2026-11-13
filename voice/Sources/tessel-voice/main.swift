// tessel-voice: listens to the microphone and turns speech into text, for
// tessel's message box.
//
// the app starts it once and keeps it; it does nothing until asked. one JSON
// object per line each way.
//
// in (stdin):
//   {"op":"start","engine":"apple","locale":"en-US"}   locale optional: the system's
//   {"op":"stop"}      finish: the last words are worked out, then "final"
//   {"op":"cancel"}    throw the take away
//
// out (stdout):
//   {"ev":"ready","engines":["apple"]}
//   {"ev":"preparing","what":"…","progress":0.4}       the first time a language is used
//   {"ev":"listening","locale":"en-US"}
//   {"ev":"level","level":0.42}                        loudness, 0…1, about 30 a second
//   {"ev":"partial","text":"…"}                        everything heard so far, still changing
//   {"ev":"final","text":"…"}                          the take, done
//   {"ev":"cancelled"}
//   {"ev":"error","message":"…"}

import Foundation
@preconcurrency import AVFoundation
import Speech

// MARK: - saying things

let outLock = NSLock()

func emit(_ fields: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: fields) else { return }
    outLock.lock()
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0a]))
    outLock.unlock()
}

func fail(_ message: String) {
    emit(["ev": "error", "message": message])
}

// MARK: - one take: from start to stop

/// the microphone, converted to what the analyzer wants, with how loud it is on the side.
final class Microphone: @unchecked Sendable {
    let engine = AVAudioEngine()
    let converter: AVAudioConverter
    let target: AVAudioFormat
    let feed: AsyncStream<AnalyzerInput>.Continuation
    private var lastLevel = Date.distantPast

    init(target: AVAudioFormat, feed: AsyncStream<AnalyzerInput>.Continuation) throws {
        self.target = target
        self.feed = feed
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        // no microphone, or no permission: the input has no format.
        guard format.sampleRate > 0 else { throw VoiceError.noMicrophone }
        guard let converter = AVAudioConverter(from: format, to: target) else { throw VoiceError.noFormat }
        self.converter = converter
        input.installTap(onBus: 0, bufferSize: 2048, format: nil) { [self] buffer, _ in
            self.level(of: buffer)
            self.pass(buffer)
        }
    }

    func start() throws {
        engine.prepare()
        try engine.start()
    }

    func stop() {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        feed.finish()
    }

    /// how loud, for the waveform: not every buffer, a steady few a second.
    private func level(of buffer: AVAudioPCMBuffer) {
        let now = Date()
        guard now.timeIntervalSince(lastLevel) > 0.033, let samples = buffer.floatChannelData?[0] else { return }
        lastLevel = now
        let n = Int(buffer.frameLength)
        guard n > 0 else { return }
        var sum: Float = 0
        for i in 0..<n {
            sum += samples[i] * samples[i]
        }
        let rms = (sum / Float(n)).squareRoot()
        // -50 dB is silence, 0 dB as loud as it gets.
        let db = 20 * log10(max(rms, 1e-7))
        emit(["ev": "level", "level": Double(min(max((db + 50) / 50, 0), 1))])
    }

    private func pass(_ buffer: AVAudioPCMBuffer) {
        let capacity = AVAudioFrameCount((Double(buffer.frameLength) * target.sampleRate / converter.inputFormat.sampleRate).rounded(.up))
        guard capacity > 0, let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
        var error: NSError?
        var given = false
        converter.convert(to: out, error: &error) { _, status in
            if given {
                status.pointee = .noDataNow
                return nil
            }
            given = true
            status.pointee = .haveData
            return buffer
        }
        if error == nil, out.frameLength > 0 {
            feed.yield(AnalyzerInput(buffer: out))
        }
    }
}

enum VoiceError: Error, CustomStringConvertible {
    case noMicrophone
    case noFormat

    var description: String {
        switch self {
        case .noMicrophone:
            return "No microphone to listen to, or no permission to. Allow Tessel in System Settings › Privacy & Security › Microphone."
        case .noFormat:
            return "The microphone's sound can't be made into what the speech engine reads."
        }
    }
}

/// a recording in progress, and what's listening to it.
final class Take {
    let mic: Microphone
    let analyzer: SpeechAnalyzer
    var results: Task<Void, Never>?
    var cancelled = false

    init(mic: Microphone, analyzer: SpeechAnalyzer) {
        self.mic = mic
        self.analyzer = analyzer
    }
}

// MARK: - the engines

@MainActor
final class Voice {
    var take: Take?

    func handle(_ line: String) async {
        guard let data = line.data(using: .utf8),
              let msg = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let op = msg["op"] as? String
        else { return }
        switch op {
        case "start":
            await start(engine: msg["engine"] as? String ?? "apple", locale: msg["locale"] as? String)
        case "stop":
            await stop()
        case "cancel":
            await cancel()
        default:
            fail("tessel-voice doesn't do \(op)")
        }
    }

    func start(engine: String, locale wanted: String?) async {
        if take != nil {
            await cancel()
        }
        guard engine == "apple" else {
            fail("tessel-voice has no engine \(engine)")
            return
        }
        guard SpeechTranscriber.isAvailable else {
            fail("This Mac's speech recognition isn't available.")
            return
        }
        guard await AVCaptureDevice.requestAccess(for: .audio) else {
            fail(VoiceError.noMicrophone.description)
            return
        }
        // the language asked for, else the Mac's own, as the engine knows it.
        let asked = wanted.map { Locale(identifier: $0) } ?? Locale.current
        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: asked) else {
            fail("Apple's speech engine doesn't know \(asked.identifier(.bcp47)). Pick another language in Settings.")
            return
        }
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        // the model for a language comes down the first time it's used.
        do {
            if !(await AssetInventory.reservedLocales).contains(where: { $0.identifier(.bcp47) == locale.identifier(.bcp47) }) {
                _ = try? await AssetInventory.reserve(locale: locale)
            }
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                let what = "Downloading Apple's speech model for \(locale.identifier(.bcp47))"
                emit(["ev": "preparing", "what": what, "progress": 0.0])
                let progress = Task {
                    while !Task.isCancelled, !request.progress.isFinished {
                        emit(["ev": "preparing", "what": what, "progress": request.progress.fractionCompleted])
                        try? await Task.sleep(for: .milliseconds(250))
                    }
                }
                try await request.downloadAndInstall()
                progress.cancel()
            }
        } catch {
            fail("Couldn't get the speech model: \(error.localizedDescription)")
            return
        }

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            fail(VoiceError.noFormat.description)
            return
        }
        let (stream, feed) = AsyncStream.makeStream(of: AnalyzerInput.self)
        let mic: Microphone
        do {
            mic = try Microphone(target: format, feed: feed)
            try mic.start()
            try await analyzer.start(inputSequence: stream)
        } catch let error as VoiceError {
            fail(error.description)
            return
        } catch {
            fail("Couldn't start listening: \(error.localizedDescription)")
            return
        }
        let take = Take(mic: mic, analyzer: analyzer)
        self.take = take
        emit(["ev": "listening", "locale": locale.identifier(.bcp47)])

        // what's settled, then what's still being worked out after it.
        take.results = Task {
            var settled = ""
            do {
                for try await result in transcriber.results {
                    let text = String(result.text.characters)
                    if result.isFinal {
                        settled += text
                        emit(["ev": "partial", "text": settled])
                    } else {
                        emit(["ev": "partial", "text": settled + text])
                    }
                }
            } catch {
                if !take.cancelled {
                    fail("Listening stopped: \(error.localizedDescription)")
                }
            }
            if !take.cancelled {
                emit(["ev": "final", "text": settled.trimmingCharacters(in: .whitespacesAndNewlines)])
            }
        }
    }

    /// done talking: the last words are worked out, then the whole of it is said once more as final.
    func stop() async {
        guard let take else { return }
        self.take = nil
        take.mic.stop()
        do {
            try await take.analyzer.finalizeAndFinishThroughEndOfInput()
        } catch {
            fail("Couldn't finish: \(error.localizedDescription)")
        }
        await take.results?.value
    }

    func cancel() async {
        guard let take else { return }
        self.take = nil
        take.cancelled = true
        take.mic.stop()
        await take.analyzer.cancelAndFinishNow()
        take.results?.cancel()
        emit(["ev": "cancelled"])
    }
}

// MARK: - the loop

let voice = Voice()
emit(["ev": "ready", "engines": SpeechTranscriber.isAvailable ? ["apple"] : []])

// stdin, a line at a time, each one handled before the next: a stop always
// follows the start it stops.
let lines = AsyncStream<String> { continuation in
    Thread.detachNewThread {
        while let line = readLine() {
            continuation.yield(line)
        }
        continuation.finish()
    }
}
for await line in lines {
    await voice.handle(line)
}
// the app went: so does this.
await voice.cancel()
exit(0)
