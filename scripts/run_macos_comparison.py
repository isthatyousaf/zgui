#!/usr/bin/env python3
"""Run the comparison workloads on this Mac.

The Linux protocol (docs/results/*/protocol.md), on macOS with the Metal GPU:
three repeats of each mode for each framework, in rotated order. Each trial
requests 20 seconds; the first 5 are excluded, the next 15 sampled every
50 ms by scripts/macsample.swift: the app's CPU time, CPU energy and
instructions, memory (resident size, and physical footprint: what Activity
Monitor reports, including GPU memory the process owns on unified memory),
its GPU time, and the window server's CPU and GPU time over the same window.
GPU time stretches as the GPU clocks down under light load, so the GPU's
mean active clock is read too (IOReport, as are the whole GPU's power):
GPU time by clock is the megacycles a second the app used.
Active trials must deliver 58-61 logical updates per requested second.

Scenes (`--scene`):
- `lab` (default): streaming text and a 100,000-row virtual list, for zgui,
  GPUI, GPUI Fast (longbridge/gpui-fast, a GPUI fork that retains unchanged
  views, layout and text) and QuickGUI.
- `heavy`: a 1280x800 dashboard where most content changes every tick
  (zgui_workload::heavy), for zgui, GPUI and GPUI Fast (QuickGUI's text
  renderer panics past 32 text batches on it).
- `animate`: dots and text animated at the display's rate
  (zgui_workload::animation), for zgui, GPUI and GPUI Fast; no update gate,
  each reports the frames it drew.

Each repeat also measures the desktop alone (`desktop`/`baseline`: a sleeping
process as the target): the window server composites every window, so its
cost with an app, less the baseline, is what the app adds. After the timed
trials, pacing trials capture each app's window (scripts/framepace.swift) to
time the frames it shows; capturing costs the window server work, so they are
kept apart from the timed trials. There are eight of those per framework and
mode: an app's 60 Hz timer lands at a phase of the display's refresh fixed for
a whole capture, near the window server's latch in some and not others, so
one capture's 99th-percentile interval swings by 5 ms or more between
captures, for every framework.

`--powermetrics` also records package and GPU power with `sudo powermetrics`
(run `sudo -v` first in the same terminal; without cached credentials the
columns stay empty).

Windows open on screen: keep the Mac awake and the windows uncovered (a
covered window may stop presenting), and keep the rest of the desktop still.
Writes docs/results/latest-macos[-<scene>]-<date>-<commit>.
"""
import argparse, csv, datetime, hashlib, json, os, plistlib, re, signal, statistics, subprocess, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REQUESTED, WARMUP, SAMPLED, INTERVAL = 20, 5, 15, 0.05
PACING_SECONDS, PACING_REPEATS = 8, 8
MODES = ['idle', 'stream', 'scroll', 'both']
SCENES = {
    'lab': {
        'modes': MODES,
        'binaries': {
            'zgui': ROOT / 'target/release/examples/component_workload',
            'gpui': ROOT / 'comparisons/gpui/target/release/zgui-compare-gpui',
            'gpui-fast': ROOT / 'comparisons/gpui-fast/target/release/zgui-compare-gpui-fast',
            'quickgui': ROOT / 'comparisons/quickgui/target/release/zgui-compare-quickgui',
        },
    },
    'heavy': {
        'modes': MODES,
        'binaries': {
            'zgui': ROOT / 'target/release/examples/heavy_workload',
            'gpui': ROOT / 'comparisons/gpui/target/release/heavy',
            'gpui-fast': ROOT / 'comparisons/gpui-fast/target/release/heavy',
        },
    },
    'animate': {
        'modes': ['animate'],
        'binaries': {
            'zgui': ROOT / 'target/release/examples/animation_workload',
            'gpui': ROOT / 'comparisons/gpui/target/release/animate',
            'gpui-fast': ROOT / 'comparisons/gpui-fast/target/release/animate',
        },
    },
}
SAMPLER, PACER = Path('/tmp/zgui-macsample'), Path('/tmp/zgui-framepace')
# What the binaries are built from, including vendored patches.
SOURCES = ['crates', 'comparisons', 'scripts', 'vendor', 'Cargo.toml', 'Cargo.lock']
METRICS = ['cpu_percent_one_core', 'cpu_watts', 'instructions', 'cycles', 'gpu_percent',
           'gpu_mcycles_per_second', 'gpu_active_mhz', 'gpu_system_watts',
           'windowserver_cpu_percent', 'windowserver_gpu_percent', 'gpu_utilization_percent',
           'mean_footprint_bytes', 'peak_footprint_bytes', 'mean_rss_bytes', 'peak_rss_bytes',
           'workload_ticks', 'wall_seconds']
POWER = ['package_mw', 'cpu_mw', 'gpu_mw']
FIELDS = ['framework', 'mode', 'repeat', 'wall_seconds', 'cpu_seconds', 'cpu_percent_one_core', 'cpu_energy_joules',
          'cpu_watts', 'instructions', 'cycles', 'gpu_seconds', 'gpu_percent', 'gpu_mcycles_per_second',
          'gpu_active_mhz', 'gpu_system_watts', 'windowserver_cpu_percent',
          'windowserver_gpu_percent', 'gpu_utilization_percent', 'mean_footprint_bytes', 'peak_footprint_bytes',
          'mean_rss_bytes', 'peak_rss_bytes', 'samples', 'workload_ticks', 'reported_fps', *POWER, 'exit_code',
          'terminated_after_report']
PACING = ['frames_per_second', 'interval_p50_ms', 'interval_p95_ms', 'interval_p99_ms', 'interval_max_ms',
          'long_interval_fraction']


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw).stdout.strip()


def build(scene):
    example = SCENES[scene]['binaries']['zgui'].name
    run(['cargo', 'build', '--release', '--locked', '-p', 'zgui-desktop', '--example', example], cwd=ROOT)
    for name in list(SCENES[scene]['binaries'])[1:]:
        run(['cargo', 'build', '--release', '--locked'], cwd=ROOT / 'comparisons' / name)
    run(['swiftc', '-O', str(ROOT / 'scripts/macsample.swift'), '-o', str(SAMPLER), '-lIOReport'])
    run(['swiftc', '-O', str(ROOT / 'scripts/framepace.swift'), '-o', str(PACER)])


def environment():
    sysctl = lambda key: run(['sysctl', '-n', key])
    power = run(['pmset', '-g', 'batt']).splitlines()[0]
    return {
        'machine': sysctl('hw.model'),
        'chip': sysctl('machdep.cpu.brand_string'),
        'cores': int(sysctl('hw.ncpu')),
        'memory_bytes': int(sysctl('hw.memsize')),
        'macos': f"{run(['sw_vers', '-productVersion'])} ({run(['sw_vers', '-buildVersion'])})",
        'power': power.split("'")[1] if "'" in power else power,
        'rust': run(['rustc', '--version']),
    }


def launch(binary, mode, seconds, log):
    env = dict(os.environ, ZGUI_MODE=mode, ZGUI_SECONDS=str(seconds))
    with log.open('w') as handle:
        process = subprocess.Popen([str(binary)], stdout=handle, stderr=subprocess.STDOUT, env=env)
    time.sleep(0.5)
    # Bring the window forward: covered windows may stop presenting.
    subprocess.run(['osascript', '-e', f'tell application "System Events" to set frontmost of (first process whose unix id is {process.pid}) to true'], capture_output=True)
    return process


def finish(process, seconds):
    try:
        return process.wait(timeout=seconds + 10), False
    except subprocess.TimeoutExpired:
        # macOS apps outlive their last window (QuickGUI closes it at the
        # deadline and reports): end it once it has reported.
        process.send_signal(signal.SIGTERM)
        return process.wait(timeout=10), True


def powermetrics():
    """Package, CPU and GPU power over the sampled window, if sudo allows."""
    command = f'sleep {WARMUP - 0.5}; exec sudo -n /usr/bin/powermetrics --samplers cpu_power,gpu_power -f plist -i 1000 -n {SAMPLED}'
    return subprocess.Popen(['/bin/sh', '-c', command], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)


def power_of(process):
    output, _ = process.communicate()
    samples = [plistlib.loads(doc.strip() + b'</plist>') for doc in output.split(b'</plist>') if doc.strip()]
    processor = [s.get('processor', {}) for s in samples]
    mean = lambda key: statistics.mean(p[key] for p in processor) if processor and all(key in p for p in processor) else ''
    return {'package_mw': mean('combined_power'), 'cpu_mw': mean('cpu_power'), 'gpu_mw': mean('gpu_power')}


def trial(scene, framework, mode, repeat, out, power):
    log = out / f'{framework}.{mode}.{repeat}.log'
    if framework == 'desktop':
        process = subprocess.Popen(['/bin/sleep', str(REQUESTED)])
    else:
        process = launch(SCENES[scene]['binaries'][framework], mode, REQUESTED, log)
    meter = powermetrics() if power else None
    sample = json.loads(run([str(SAMPLER), str(process.pid), str(WARMUP - 0.5), str(SAMPLED), str(INTERVAL)]))
    exit_code, terminated = finish(process, REQUESTED)
    text = log.read_text() if log.exists() else ''
    ticks = re.search(r'"ticks":(\d+)|workload_ticks=(\d+)|animation_frames=(\d+)', text)
    fps = re.search(r'fps=([0-9.]+)', text)
    row = {'framework': framework, 'mode': mode, 'repeat': repeat, **sample,
           'workload_ticks': int(next(g for g in ticks.groups() if g)) if ticks else (0 if framework == 'desktop' else -1),
           'reported_fps': float(fps.group(1)) if fps else '',
           **(power_of(meter) if meter else dict.fromkeys(POWER, '')),
           'exit_code': exit_code, 'terminated_after_report': terminated}
    (out / f'{framework}.{mode}.{repeat}.json').write_text(json.dumps(row, indent=1) + '\n')
    return row


def pacing(scene, framework, mode, repeat, out):
    seconds = WARMUP + PACING_SECONDS + 1
    # Screen capture occasionally fails to find a window that just opened:
    # one more try before giving up on the trial.
    for attempt in range(2):
        process = launch(SCENES[scene]['binaries'][framework], mode, seconds, out / f'{framework}.{mode}.{repeat}.pacing.log')
        try:
            result = json.loads(run([str(PACER), str(process.pid), str(WARMUP - 0.5), str(PACING_SECONDS)]))
            break
        except subprocess.CalledProcessError:
            finish(process, seconds)
            if attempt:
                raise
            time.sleep(2)
    finish(process, seconds)
    row = {'framework': framework, 'mode': mode, 'repeat': repeat, **result}
    (out / f'{framework}.{mode}.{repeat}.pacing.json').write_text(json.dumps(row, indent=1) + '\n')
    return row


def source_diff_sha256():
    """The uncommitted source: tracked changes and untracked files."""
    digest = hashlib.sha256(run(['git', 'diff', 'HEAD', '--', *SOURCES], cwd=ROOT).encode())
    for path in sorted(run(['git', 'ls-files', '--others', '--exclude-standard', '--', *SOURCES], cwd=ROOT).split()):
        digest.update(path.encode() + b'\0' + (ROOT / path).read_bytes())
    return digest.hexdigest()


def group(rows, keys, framework, mode):
    chosen = [r for r in rows if (r['framework'], r['mode']) == (framework, mode)]
    stats = {}
    for key in keys:
        values = [float(r[key]) for r in chosen if r.get(key) not in ('', None)]
        if values:
            stats[key] = {'median': statistics.median(values), 'min': min(values), 'max': max(values)}
    return {'framework': framework, 'mode': mode, 'repeats': len(chosen), **stats}


def write_csv(path, fields, rows):
    with path.open('w', newline='') as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, extrasaction='ignore')
        writer.writeheader()
        writer.writerows(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--skip-build', action='store_true')
    parser.add_argument('--scene', choices=sorted(SCENES), default='lab')
    parser.add_argument('--powermetrics', action='store_true')
    args = parser.parse_args()
    frameworks, modes = list(SCENES[args.scene]['binaries']), SCENES[args.scene]['modes']
    if not args.skip_build:
        build(args.scene)
    commit = run(['git', 'rev-parse', 'HEAD'], cwd=ROOT)
    started = datetime.datetime.now(datetime.timezone.utc)
    scene = '' if args.scene == 'lab' else f'-{args.scene}'
    out = ROOT / 'docs/results' / f'latest-macos{scene}-{started:%Y-%m-%d}-{commit[:7]}'
    out.mkdir(parents=True, exist_ok=True)
    rows, paced = [], []
    for repeat in range(3):
        order = frameworks[repeat:] + frameworks[:repeat]
        for framework, mode in [('desktop', 'baseline')] + [(f, m) for m in modes for f in order]:
            row = trial(args.scene, framework, mode, repeat, out, args.powermetrics)
            print(f"{framework:8} {mode:8} {repeat}: cpu {row['cpu_percent_one_core']:6.2f}% {row['cpu_watts'] * 1000:6.0f} mW "
                  f"gpu {row['gpu_percent']:5.2f}% {row['gpu_mcycles_per_second']:5.1f} Mcyc/s server cpu {row['windowserver_cpu_percent']:5.1f}% gpu {row['windowserver_gpu_percent']:5.1f}% "
                  f"footprint {row['mean_footprint_bytes'] / 2**20:6.1f} MiB ticks {row['workload_ticks']}", flush=True)
            rows.append(row)
            time.sleep(2)
    for repeat in range(PACING_REPEATS):
        order = frameworks[repeat % len(frameworks):] + frameworks[:repeat % len(frameworks)]
        for mode in [m for m in modes if m != 'idle']:
            for framework in order:
                row = pacing(args.scene, framework, mode, repeat, out)
                print(f"{framework:8} {mode:8} {repeat}: pacing {row.get('frames_per_second', 0):6.1f} fps p99 "
                      f"{row.get('interval_p99_ms', 0):5.1f} ms long {row.get('long_interval_fraction', 0):.3f}", flush=True)
                paced.append(row)
                time.sleep(2)
    write_csv(out / 'current.csv', FIELDS, rows)
    write_csv(out / 'pacing.csv', ['framework', 'mode', 'repeat', 'display_hz', 'frames', *PACING, 'long_intervals'], paced)
    groups = [group(rows, METRICS + POWER, 'desktop', 'baseline')]
    groups += [group(rows, METRICS + POWER + ['reported_fps'], f, m) for f in frameworks for m in modes]
    pacing_groups = [group(paced, PACING, f, m) for f in frameworks for m in modes if m != 'idle']
    (out / 'summary.json').write_text(json.dumps({'groups': groups, 'pacing': pacing_groups}, indent=1) + '\n')
    apps = [r for r in rows if r['framework'] != 'desktop']
    if args.scene == 'animate':
        active = [r['reported_fps'] for r in apps if r['reported_fps'] != '']
        gated = all(r['workload_ticks'] > 0 for r in apps)
    else:
        active = [r['workload_ticks'] / REQUESTED for r in apps if r['mode'] != 'idle']
        gated = (all(58 <= t <= 61 for t in active)
                 and all(r['workload_ticks'] == 0 for r in apps if r['mode'] == 'idle'))
    accepted = gated and all(r['exit_code'] == 0 or r['terminated_after_report'] for r in apps)
    binaries = {name: {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'bytes': path.stat().st_size}
                for name, path in SCENES[args.scene]['binaries'].items()}
    audit = {'accepted': accepted, 'trials': len(rows), 'samples': sum(r['samples'] for r in rows),
             'pacing_trials': len(paced),
             ('reported_fps' if args.scene == 'animate' else 'active_ticks_per_requested_second'):
                 {'min': min(active), 'max': max(active)} if active else None,
             'powermetrics': any(r.get('package_mw') not in ('', None) for r in rows),
             'scene': args.scene, 'commit': commit,
             # Uncommitted source changes the binaries were built with, if any.
             'source_dirty': bool(run(['git', 'status', '--porcelain', '--', *SOURCES], cwd=ROOT)),
             'source_diff_sha256': source_diff_sha256(),
             'started_utc': started.isoformat(), 'finished_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
             'environment': environment(), 'binaries': binaries,
             'protocol': {'requested_seconds': REQUESTED, 'warmup_seconds': WARMUP, 'sampled_seconds': SAMPLED,
                          'interval_seconds': INTERVAL, 'repeats': 3, 'frameworks': frameworks, 'modes': modes,
                          'pacing_seconds': PACING_SECONDS, 'pacing_repeats': PACING_REPEATS,
                          'acceptance_updates_per_second': [58, 61]}}
    (out / 'audit.json').write_text(json.dumps(audit, indent=1) + '\n')
    print(('ACCEPTED' if accepted else 'REJECTED'), out)


if __name__ == '__main__':
    main()
