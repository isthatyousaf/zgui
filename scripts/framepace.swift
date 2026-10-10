import CoreMedia
import Foundation
import ScreenCaptureKit
// framepace <pid> <delay> <seconds>: how evenly a process's window shows new
// frames. Captures its largest on-screen window with ScreenCaptureKit at the
// display's rate (scaled down, which costs little and still sees every
// change) and times each frame whose pixels changed. Framework-neutral: it
// sees what the window server composites, not what the app reports. The
// capture itself costs the window server some work, so measure CPU and power
// in separate runs. Needs Screen Recording permission. JSON on stdout.
let a = CommandLine.arguments
let pid = pid_t(a[1])!, delay = Double(a[2])!, seconds = Double(a[3])!
var tb = mach_timebase_info_data_t(); mach_timebase_info(&tb)

final class Recorder: NSObject, SCStreamOutput {
  var times: [Double] = []
  var recording = false
  var last: UInt64 = 0
  let lock = NSLock()
  func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
    guard type == .screen,
          let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
          let raw = attachments.first?[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
          let display = attachments.first?[.displayTime] as? UInt64
    else { return }
    // Frames arrive whenever the window server recomposites the window,
    // changed or not (translucency over an animated desktop, say): only those
    // whose pixels differ from the last count. A sparse grid is enough.
    guard let pixels = CMSampleBufferGetImageBuffer(buffer) else { return }
    CVPixelBufferLockBaseAddress(pixels, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddress(pixels) else { return }
    let (width, height, stride) = (CVPixelBufferGetWidth(pixels), CVPixelBufferGetHeight(pixels), CVPixelBufferGetBytesPerRow(pixels))
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for y in Swift.stride(from: 0, to: height, by: 2) {
      let row = base.advanced(by: y * stride).assumingMemoryBound(to: UInt32.self)
      for x in Swift.stride(from: 0, to: width, by: 2) {
        hash = (hash ^ UInt64(row[x])) &* 0x100_0000_01b3
      }
    }
    lock.withLock {
      defer { last = hash }
      if recording && hash != last { times.append(Double(display) * Double(tb.numer) / Double(tb.denom) / 1e9) }
    }
  }
}

func fail(_ message: String) -> Never {
  print("{\"error\":\"\(message)\"}"); exit(1)
}

let recorder = Recorder()
Task {
  do {
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    guard let window = content.windows
      .filter({ $0.owningApplication?.processID == pid })
      .max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height })
    else { fail("no window") }
    let rate = NSScreen.screens.map(\.maximumFramesPerSecond).max() ?? 60
    let config = SCStreamConfiguration()
    // The content below the title bar, which may be translucent.
    let titleBar: CGFloat = 32
    config.sourceRect = CGRect(x: 0, y: titleBar, width: window.frame.width, height: window.frame.height - titleBar)
    config.width = Int(window.frame.width / 2)
    config.height = Int((window.frame.height - titleBar) / 2)
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(rate))
    config.queueDepth = 8
    config.showsCursor = false
    let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: window), configuration: config, delegate: nil)
    try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: DispatchQueue(label: "frames"))
    try await stream.startCapture()
    try await Task.sleep(nanoseconds: UInt64(delay * 1e9))
    recorder.lock.withLock { recorder.recording = true }
    try await Task.sleep(nanoseconds: UInt64(seconds * 1e9))
    let times = recorder.lock.withLock {
      recorder.recording = false
      return recorder.times
    }
    try await stream.stopCapture()
    let intervals = zip(times.dropFirst(), times).map { ($0 - $1) * 1000 }.sorted()
    let at = { (q: Double) in intervals.isEmpty ? 0 : intervals[min(intervals.count - 1, Int(q * Double(intervals.count)))] }
    // A 60 Hz update shows every 16.7 ms; past 25 ms one was missed.
    let long = intervals.filter { $0 > 25 }.count
    print(String(format: "{\"display_hz\":%d,\"frames\":%d,\"frames_per_second\":%.3f,\"interval_p50_ms\":%.3f,\"interval_p95_ms\":%.3f,\"interval_p99_ms\":%.3f,\"interval_max_ms\":%.3f,\"long_intervals\":%d,\"long_interval_fraction\":%.5f}",
      rate, times.count, Double(times.count) / seconds, at(0.5), at(0.95), at(0.99), intervals.last ?? 0, long,
      intervals.isEmpty ? 0 : Double(long) / Double(intervals.count)))
    exit(0)
  } catch {
    fail("\(error)".replacingOccurrences(of: "\"", with: "'"))
  }
}
RunLoop.main.run()
