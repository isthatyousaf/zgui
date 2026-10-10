import Foundation
import IOKit
// macsample <pid> <delay> <seconds> <interval>: a process's CPU time, CPU
// energy, instructions, memory and GPU time between delay and delay+seconds,
// polled every interval, and what the window server spent meanwhile (its CPU
// and GPU time; it composites every app's windows, so run on a quiet
// desktop). GPU time stretches as the GPU clocks down under light load, so
// the GPU's mean active clock comes too: time by clock counts the cycles a
// process used. JSON on stdout. Nothing here needs root.
let a = CommandLine.arguments
let pid = pid_t(a[1])!, delay = Double(a[2])!, seconds = Double(a[3])!, interval = Double(a[4])!
var tb = mach_timebase_info_data_t(); mach_timebase_info(&tb)
func sample() -> rusage_info_v6? {
  var info = rusage_info_v6()
  let ok = withUnsafeMutablePointer(to: &info) { p in
    p.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) { proc_pid_rusage(pid, RUSAGE_INFO_V6, $0) }
  }
  return ok == 0 ? info : nil
}
let ns = { (t: UInt64) in Double(t) * Double(tb.numer) / Double(tb.denom) / 1e9 }
func run(_ path: String, _ args: [String]) -> String {
  let p = Process(); p.executableURL = URL(fileURLWithPath: path); p.arguments = args
  let out = Pipe(); p.standardOutput = out; p.standardError = FileHandle.nullDevice
  try? p.run(); p.waitUntilExit()
  return String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
}
// The window server belongs to another user: `ps` reads its CPU time
// ([[dd-]hh:]mm:ss.ss), which proc_pid_rusage may not.
let windowServer = pid_t(run("/usr/bin/pgrep", ["-x", "WindowServer"]).split(separator: "\n").first.map(String.init) ?? "") ?? 0
func cpuSeconds(_ pid: pid_t) -> Double {
  let text = run("/bin/ps", ["-o", "time=", "-p", String(pid)]).trimmingCharacters(in: .whitespacesAndNewlines)
  let days = text.split(separator: "-")
  let clock = days.last.map(String.init) ?? ""
  let d = days.count > 1 ? Double(days[0]) ?? 0 : 0
  return clock.split(separator: ":").reduce(0) { $0 * 60 + (Double($1) ?? 0) } + d * 86400
}
// GPU time per process, as Activity Monitor's GPU Time: each process's GPU
// clients in the I/O Registry count the time its command buffers ran. Keyed
// by client: a process may close a client and open another.
struct Client: Hashable { let id: UInt64; let pid: pid_t }
func gpuClients() -> [Client: Double] {
  // The walk ends early, silently, if the registry changes under it (any
  // process opening or closing a client): scan again until one completes.
  var clients: [Client: Double] = [:]
  for _ in 0..<20 {
    var iterator: io_iterator_t = 0
    guard IORegistryCreateIterator(kIOMainPortDefault, kIOServicePlane, IOOptionBits(kIORegistryIterateRecursively), &iterator) == KERN_SUCCESS else { return [:] }
    clients = scan(iterator)
    let complete = IOIteratorIsValid(iterator) != 0
    IOObjectRelease(iterator)
    if complete { return clients }
  }
  return clients
}
func scan(_ iterator: io_iterator_t) -> [Client: Double] {
  var clients: [Client: Double] = [:]
  var entry = IOIteratorNext(iterator)
  while entry != 0 {
    var id: UInt64 = 0
    if IOObjectConformsTo(entry, "IOUserClient") != 0,
       IORegistryEntryGetRegistryEntryID(entry, &id) == KERN_SUCCESS,
       let usage = IORegistryEntryCreateCFProperty(entry, "AppUsage" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? [[String: Any]],
       let creator = IORegistryEntryCreateCFProperty(entry, "IOUserClientCreator" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? String,
       let owner = pid_t(creator.dropFirst(4).prefix { $0 != "," }) {
      let time = usage.compactMap { ($0["accumulatedGPUTime"] as? NSNumber)?.doubleValue }.reduce(0, +)
      clients[Client(id: id, pid: owner)] = time / 1e9
    }
    IOObjectRelease(entry); entry = IOIteratorNext(iterator)
  }
  return clients
}
/// GPU seconds `pid`'s clients ran between two scans. A client opened in
/// between counts from zero; one closed in between loses what it ran after
/// the first scan, so this can only undercount.
func gpuDelta(_ before: [Client: Double], _ after: [Client: Double], _ pid: pid_t) -> Double {
  after.filter { $0.key.pid == pid }.reduce(0) { $0 + max(0, $1.value - (before[$1.key] ?? 0)) }
}
@_silgen_name("IOReportCopyChannelsInGroup") func IOReportCopyChannelsInGroup(_ group: CFString?, _ subgroup: CFString?, _ a: UInt64, _ b: UInt64, _ c: UInt64) -> Unmanaged<CFMutableDictionary>?
@_silgen_name("IOReportMergeChannels") func IOReportMergeChannels(_ a: CFMutableDictionary, _ b: CFMutableDictionary, _ c: CFTypeRef?)
@_silgen_name("IOReportCreateSubscription") func IOReportCreateSubscription(_ a: UnsafeMutableRawPointer?, _ channels: CFMutableDictionary, _ subbed: UnsafeMutablePointer<Unmanaged<CFMutableDictionary>?>, _ id: UInt64, _ b: CFTypeRef?) -> OpaquePointer?
@_silgen_name("IOReportCreateSamples") func IOReportCreateSamples(_ sub: OpaquePointer, _ channels: CFMutableDictionary, _ a: CFTypeRef?) -> Unmanaged<CFDictionary>?
@_silgen_name("IOReportCreateSamplesDelta") func IOReportCreateSamplesDelta(_ a: CFDictionary, _ b: CFDictionary, _ c: CFTypeRef?) -> Unmanaged<CFDictionary>?
@_silgen_name("IOReportChannelGetGroup") func IOReportChannelGetGroup(_ ch: CFDictionary) -> Unmanaged<CFString>?
@_silgen_name("IOReportChannelGetSubGroup") func IOReportChannelGetSubGroup(_ ch: CFDictionary) -> Unmanaged<CFString>?
@_silgen_name("IOReportChannelGetChannelName") func IOReportChannelGetChannelName(_ ch: CFDictionary) -> Unmanaged<CFString>?
@_silgen_name("IOReportChannelGetUnitLabel") func IOReportChannelGetUnitLabel(_ ch: CFDictionary) -> Unmanaged<CFString>?
@_silgen_name("IOReportSimpleGetIntegerValue") func IOReportSimpleGetIntegerValue(_ ch: CFDictionary, _ i: Int32) -> Int64
@_silgen_name("IOReportStateGetCount") func IOReportStateGetCount(_ ch: CFDictionary) -> Int32
@_silgen_name("IOReportStateGetResidency") func IOReportStateGetResidency(_ ch: CFDictionary, _ i: Int32) -> Int64
// Whole-GPU energy and clock: IOReport's "Energy Model" and the GPU's
// performance-state residency, with the state frequencies from the power
// manager (voltage-states9: frequency and voltage pairs, OFF first).
struct GpuPower {
  let subscription: OpaquePointer
  let channels: CFMutableDictionary
  let mhz: [Double]
  init?() {
    guard let energy = IOReportCopyChannelsInGroup("Energy Model" as CFString, nil, 0, 0, 0)?.takeRetainedValue(),
          let stats = IOReportCopyChannelsInGroup("GPU Stats" as CFString, nil, 0, 0, 0)?.takeRetainedValue() else { return nil }
    IOReportMergeChannels(energy, stats, nil)
    var subbed: Unmanaged<CFMutableDictionary>?
    guard let sub = IOReportCreateSubscription(nil, energy, &subbed, 0, nil), let channels = subbed?.takeRetainedValue() else { return nil }
    subscription = sub; self.channels = channels
    let pmgr = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceNameMatching("pmgr"))
    defer { IOObjectRelease(pmgr) }
    let data = IORegistryEntryCreateCFProperty(pmgr, "voltage-states9" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? Data ?? Data()
    mhz = data.withUnsafeBytes { raw in stride(from: 0, to: raw.count / 8, by: 1).map { Double(raw.load(fromByteOffset: $0 * 8, as: UInt32.self)) / 1e6 } }
  }
  func sample() -> CFDictionary? { IOReportCreateSamples(subscription, channels, nil)?.takeRetainedValue() }
  /// GPU joules and mean active MHz between two samples.
  func between(_ a: CFDictionary, _ b: CFDictionary) -> (joules: Double, mhz: Double) {
    guard let delta = IOReportCreateSamplesDelta(a, b, nil)?.takeRetainedValue() as? [String: Any],
          let list = delta["IOReportChannels"] as? [CFDictionary] else { return (0, 0) }
    var joules = 0.0, active = 0.0, weighted = 0.0
    for ch in list {
      let group = IOReportChannelGetGroup(ch)?.takeUnretainedValue() as String? ?? ""
      let name = IOReportChannelGetChannelName(ch)?.takeUnretainedValue() as String? ?? ""
      if group == "Energy Model" && name == "GPU Energy" {
        let unit = IOReportChannelGetUnitLabel(ch)?.takeUnretainedValue() as String? ?? ""
        let scale: Double = ["nJ": 1e-9, "uJ": 1e-6, "mJ": 1e-3][unit] ?? 1
        joules += Double(IOReportSimpleGetIntegerValue(ch, 0)) * scale
      } else if group == "GPU Stats", name == "GPUPH",
                IOReportChannelGetSubGroup(ch)?.takeUnretainedValue() as String? == "GPU Performance States" {
        for k in 1..<min(Int(IOReportStateGetCount(ch)), mhz.count) {
          let r = Double(IOReportStateGetResidency(ch, Int32(k)))
          active += r; weighted += r * mhz[k]
        }
      }
    }
    return (joules, active > 0 ? weighted / active : 0)
  }
}
// Whole-GPU utilization, all processes (the accelerator's own statistic).
let accelerator = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("IOAccelerator"))
func gpuUtilization() -> Double? {
  guard accelerator != 0,
        let stats = IORegistryEntryCreateCFProperty(accelerator, "PerformanceStatistics" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? [String: Any]
  else { return nil }
  return (stats["Device Utilization %"] as? NSNumber)?.doubleValue
}

Thread.sleep(forTimeInterval: delay)
// GPU and window-server time are read by scans, which find nothing of a
// process that has quit (a workload may quit right at the window's end): a
// scan each second, and those rates run to the last one that still found the
// process's GPU clients (any scan, for a process that has none).
let power = GpuPower()
let gpuStart = Date()
let powerBefore = power?.sample()
let (gpuBefore, serverCpuBefore) = (gpuClients(), cpuSeconds(windowServer))
let hadGpu = gpuBefore.keys.contains { $0.pid == pid }
guard let first = sample() else { print("{\"error\":\"exited\"}"); exit(1) }
let start = Date()
var footprints: [Double] = [], residents: [Double] = [], utilization: [Double] = []
var last = first
var closing: (gpu: [Client: Double], serverCpu: Double, at: Date, power: CFDictionary?)?
var scanned = start
while Date().timeIntervalSince(start) < seconds {
  Thread.sleep(forTimeInterval: interval)
  guard let s = sample() else { break }
  footprints.append(Double(s.ri_phys_footprint)); residents.append(Double(s.ri_resident_size)); last = s
  if let u = gpuUtilization() { utilization.append(u) }
  if Date().timeIntervalSince(scanned) >= 1 {
    scanned = Date()
    let clients = gpuClients()
    if !hadGpu || clients.keys.contains(where: { $0.pid == pid }) {
      closing = (clients, cpuSeconds(windowServer), Date(), power?.sample())
    }
  }
}
let wall = Date().timeIntervalSince(start)
let (gpuAfter, serverCpuAfter, gpuEnd, powerAfter) = closing ?? (gpuClients(), cpuSeconds(windowServer), Date(), power?.sample())
let gpuWall = gpuEnd.timeIntervalSince(gpuStart)
let cpu = ns(last.ri_user_time - first.ri_user_time) + ns(last.ri_system_time - first.ri_system_time)
let energy = Double(last.ri_energy_nj - first.ri_energy_nj) / 1e9
let gpu = gpuDelta(gpuBefore, gpuAfter, pid)
let serverGpu = gpuDelta(gpuBefore, gpuAfter, windowServer)
let serverCpu = serverCpuAfter - serverCpuBefore
let (gpuJoules, gpuMhz) = { () -> (Double, Double) in
  guard let power, let powerBefore, let powerAfter else { return (0, 0) }
  return power.between(powerBefore, powerAfter)
}()
let mean = { (v: [Double]) in v.isEmpty ? 0 : v.reduce(0, +) / Double(v.count) }
print(String(format: "{\"wall_seconds\":%.4f,\"cpu_seconds\":%.4f,\"cpu_percent_one_core\":%.4f,\"cpu_energy_joules\":%.4f,\"cpu_watts\":%.5f,\"instructions\":%llu,\"cycles\":%llu,\"gpu_seconds\":%.5f,\"gpu_percent\":%.4f,\"windowserver_cpu_percent\":%.4f,\"windowserver_gpu_percent\":%.4f,\"gpu_utilization_percent\":%.3f,\"gpu_active_mhz\":%.1f,\"gpu_mcycles_per_second\":%.3f,\"gpu_system_watts\":%.4f,\"mean_footprint_bytes\":%.0f,\"peak_footprint_bytes\":%.0f,\"mean_rss_bytes\":%.0f,\"peak_rss_bytes\":%.0f,\"samples\":%d}",
  wall, cpu, cpu / wall * 100, energy, energy / wall, last.ri_instructions - first.ri_instructions, last.ri_cycles - first.ri_cycles,
  gpu, gpu / gpuWall * 100, serverCpu / gpuWall * 100, serverGpu / gpuWall * 100, mean(utilization),
  gpuMhz, gpu * gpuMhz / gpuWall, gpuJoules / gpuWall,
  mean(footprints), footprints.max() ?? 0, mean(residents), residents.max() ?? 0, footprints.count))
