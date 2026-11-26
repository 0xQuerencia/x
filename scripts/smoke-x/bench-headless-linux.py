#!/usr/bin/env python3
"""Measure an isolated Linux preview process with audio and desktop access off."""

import argparse
import collections
import signal
import json
import math
import os
import pathlib
import statistics
import subprocess
import tempfile
import time

arguments = argparse.ArgumentParser(description=__doc__)
arguments.add_argument('binary', type=pathlib.Path)
arguments.add_argument('--network-trace', action='store_true',
                       help='Observe IP and Unix socket syscalls with strace instead of timing.')
options = arguments.parse_args()
binary = str(options.binary.resolve(strict=True))
trace = options.network_trace
clock_ticks = os.sysconf('SC_CLK_TCK')

def process_sample(pid):
    proc = pathlib.Path('/proc') / str(pid)
    status = dict(line.split(':', 1) for line in (proc / 'status').read_text().splitlines())
    fields = (proc / 'stat').read_text().rsplit(') ', 1)[1].split()
    return {
        'rss_kib': int(status['VmRSS'].split()[0]),
        'threads': int(status['Threads']),
        'fds': len(list((proc / 'fd').iterdir())),
        'cpu_seconds': (int(fields[11]) + int(fields[12])) / clock_ticks,
    }

def report(values):
    ordered = sorted(values)
    return {
        'n': len(values),
        'median_ms': round(statistics.median(values), 3),
        'p95_ms': round(ordered[math.ceil(len(ordered) * .95) - 1], 3),
    }

with tempfile.TemporaryDirectory(prefix='x-baseline-') as directory:
    root = pathlib.Path(directory)
    for folder in ('data', 'runtime', 'home'):
        (root / folder).mkdir(mode=0o700)
    (root / 'data' / 'settings.json').write_text(
        json.dumps({'enable_on_launch': False, 'labs': {'x_agent': True}})
    )
    env = {
        'PATH': '/usr/bin:/bin',
        'LANG': 'C.UTF-8',
        'HOME': str(root / 'home'),
        'HEX_APPLICATION_SUPPORT_DIR': str(root / 'data'),
        'XDG_RUNTIME_DIR': str(root / 'runtime'),
        'XDG_DATA_HOME': str(root / 'xdg-data'),
        'XDG_CONFIG_HOME': str(root / 'xdg-config'),
        'XDG_CACHE_HOME': str(root / 'xdg-cache'),
        'ALSA_CONFIG_PATH': '/dev/null',
    }
    prefix = ['strace', '-ff', '-e', 'trace=%network', '-o', str(root / 'network')] if trace else []

    def cli(*args):
        response = subprocess.run([*prefix_for_cli(), binary, *args], env=env,
                                  capture_output=True, text=True, timeout=10)
        response.check_returncode()
        return response.stdout

    def prefix_for_cli():
        # strace writes one file per traced PID, even with the same prefix.
        return [*prefix[:-1], str(root / 'network-client')] if trace else []

    def wait_for_socket():
        socket = root / 'data' / 'service.sock'
        deadline = time.monotonic() + 8
        while not socket.is_socket():
            if time.monotonic() >= deadline:
                raise TimeoutError('service socket did not become ready')
            time.sleep(.002)

    def measure(func, count):
        times = []
        for _ in range(count):
            start = time.perf_counter_ns()
            cleanup = func()
            times.append((time.perf_counter_ns() - start) / 1e6)
            if cleanup is not None:
                cleanup()
        return report(times)

    def launch():
        proc = subprocess.Popen([*prefix, binary, 'app', '--hidden'], env=env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, start_new_session=True)
        return proc

    def stop(proc):
        try:
            cli('quit')
            if proc.wait(timeout=7) != 0:
                raise RuntimeError('service did not exit successfully')
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=2)

    def fresh():
        proc = launch()
        try:
            pid = wait_ready()
            assert trace or pid == proc.pid
        except BaseException:
            stop(proc)
            raise
        return lambda: stop(proc)

    def wait_ready():
        wait_for_socket()
        deadline = time.monotonic() + 8
        while True:
            try:
                return read_status()
            except subprocess.CalledProcessError as error:
                if time.monotonic() >= deadline:
                    raise RuntimeError(error.stderr) from error
                time.sleep(.01)

    def read_status():
        status = json.loads(cli('status', '--json'))
        assert status['status'] == 'status' and not status['listener']['running']
        return status['pid']

    def status():
        assert read_status() == pid

    def preview():
        response = json.loads(cli('agent', 'say', '--mode', 'actions', '--json', 'list windows'))
        snapshot = response['snapshot']
        task = snapshot['task']
        assert (response['status'], snapshot['phase'], task['text']) == (
            'agent', 'awaiting_approval', 'list windows'
        )
        return lambda: cli('agent', 'stop', '--task', str(task['id']))

    results = {}
    if not trace:
        results['fresh_process_status'] = measure(fresh, 20)
    daemon = launch()
    try:
        pid = wait_ready()
        settings = json.loads(cli('agent', 'status', '--json'))
        assert settings['snapshot']['trusted_actions'] is False
        if trace:
            preview()()
        else:
            time.sleep(1)
            results['idle_before'] = process_sample(pid)
            results['status'] = measure(status, 100)
            results['local_preview'] = measure(preview, 100)
            time.sleep(1)
            results['idle_after_warmup'] = process_sample(pid)
            started = time.monotonic()
            time.sleep(8)
            after_idle = process_sample(pid)
            results['idle_cpu_percent'] = round(
                (after_idle['cpu_seconds'] - results['idle_after_warmup']['cpu_seconds'])
                / (time.monotonic() - started) * 100, 3
            )
            for _ in range(500):
                status()
            for _ in range(300):
                preview()()
            time.sleep(1)
            results['idle_after_cycles'] = process_sample(pid)
    finally:
        stop(daemon)

    if trace:
        lines = [line for path in root.glob('network*') for line in path.read_text().splitlines()]
        results['ip_connects'] = sum('connect(' in line and 'sa_family=AF_INET' in line for line in lines)
        endpoints = collections.Counter(
            line.rsplit('connect(', 1)[-1].split(') = ', 1)[0].split(', ', 1)[-1]
            for line in lines if 'connect(' in line and 'sa_family=AF_INET' in line
        )
        results['ip_connect_targets'] = dict(endpoints)
        results['ip_binds'] = sum('bind(' in line and 'sa_family=AF_INET' in line for line in lines)
        results['ip_sends'] = sum('sa_family=AF_INET' in line and any(line.startswith(call)
                                  for call in ('sendto(', 'sendmsg(', 'sendmmsg(')) for line in lines)
        results['unix_connects'] = sum('connect(' in line and 'sa_family=AF_UNIX' in line for line in lines)
        results['syscalls'] = len(lines)
    print(json.dumps(results, indent=2))
