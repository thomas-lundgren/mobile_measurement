#!/usr/bin/env python3
"""Start one hol_capped.sh + agent cycle per commute, without anyone asking.

Runs during the weekday commute windows (cron starts it every minute; flock
keeps one copy) and samples the phone's status every 15 s: where it reports
from (server-main's "reports from <ip> (local|remote)" line; local = home
Wi-Fi) and its serving cell (status.radio, Logger 17:02 build onward).

Starts the cycle once per window when the phone is IDLE on the mobile network
and either
  - morning: it was on home Wi-Fi within the last LEFT_HOME_MIN minutes
    (just left home), or
  - evening: it has seen >= MOVING_CELLS different cells within the last
    MOVING_MIN minutes (moving) and has not been home for the last
    AWAY_MIN minutes (so a dog walk from home doesn't count).

hol_capped.sh, then the agent phases of app_test.sh (AGENT_PHASES), run with
CELL_ONLY=1 WAIT_MIN=3: each stops at the first phase the phone can't start
(e.g. home Wi-Fi reached). Uploads stay held until home.

Off switch: touch ~/mp-data/commute_watch.off
Log: ~/mp-data/logs/commute_watch.log
"""

import json
import os
import sqlite3
import subprocess
import sys
import time
from datetime import datetime

DATA = os.path.expanduser("~/mp-data")
DEVICE = "dev_01M588ENHCVDGS8DZ9B5JCN7EP"
WINDOWS = {"am": ("06:30", "09:30"), "pm": ("15:00", "19:00")}  # Mon–Fri
SAMPLE_S = 15
# A status counts while younger than this: the Logger reports every 100 s when
# idle and at once when its state, network or serving cell changes.
FRESH_S = 300
LEFT_HOME_MIN = 15
MOVING_CELLS, MOVING_MIN = 3, 5
AWAY_MIN = 60
AGENT_PHASES = "app-agent-device app-agent-cloud app-agent-hybrid"
HERE = os.path.dirname(os.path.abspath(__file__))  # the cycle scripts
STATE = os.path.join(DATA, "commute_watch")
LOG = os.path.join(DATA, "logs", "commute_watch.log")


def log(msg):
    with open(LOG, "a") as f:
        f.write(f"{datetime.now():%F %T} {msg}\n")


def window(now):
    if now.isoweekday() > 5:
        return None
    hm = now.strftime("%H:%M")
    for name, (a, b) in WINDOWS.items():
        if a <= hm < b:
            return name
    return None


def network():
    """'local' / 'remote' from server-main's last "reports from" line."""
    last = None
    with open(os.path.join(DATA, "logs", "server-main.log"), errors="replace") as f:
        for line in f:
            if line.startswith(f"status: {DEVICE} reports from"):
                last = line
    if last is None:
        return None
    return "local" if last.rstrip().endswith("(local)") else "remote"


def status():
    """(logger_state, age_s, cell key or None)."""
    c = sqlite3.connect(os.path.join(DATA, "mp.db"))
    try:
        r = c.execute("select logger_state, received_at, radio_json from device_status where device_id=?",
                      (DEVICE,)).fetchone()
    finally:
        c.close()
    if r is None:
        return None, None, None
    cell = None
    if r[2]:
        radio = json.loads(r[2])
        if radio.get("pci") is not None:
            cell = f"{radio['pci']}/{radio.get('nci', '-')}"
    return r[0], time.time() - r[1] / 1e9, cell


def main():
    os.makedirs(STATE, exist_ok=True)
    samples_path = os.path.join(STATE, "samples.log")  # "epoch net cell"
    last_cell = None
    while True:
        now = datetime.now()
        win = window(now)
        if win is None or os.path.exists(os.path.join(DATA, "commute_watch.off")):
            return
        done = os.path.join(STATE, f"done_{now:%F}_{win}")
        if os.path.exists(done):
            return

        state, age, cell = status()
        net = network()
        t = time.time()
        if age is not None and age < FRESH_S and net:
            with open(samples_path, "a") as f:
                f.write(f"{int(t)} {net} {cell or '-'}\n")
            if cell != last_cell:
                log(f"{win}: {net} cell {cell}")
                last_cell = cell

        samples = []
        with open(samples_path) as f:
            for line in f:
                p = line.split()
                if len(p) == 3 and t - int(p[0]) < AWAY_MIN * 60:
                    samples.append((int(p[0]), p[1], p[2]))

        fresh = age is not None and age < FRESH_S
        reason = None
        if fresh and state == "IDLE" and net == "remote":
            if win == "am" and any(n == "local" and t - s < LEFT_HOME_MIN * 60 for s, n, _ in samples):
                reason = "left home Wi-Fi"
            elif win == "pm" and not any(n == "local" for _, n, _ in samples):
                cells = {c for s, _, c in samples if c != "-" and t - s < MOVING_MIN * 60}
                if len(cells) >= MOVING_CELLS:
                    reason = f"moving ({len(cells)} cells in {MOVING_MIN} min: {' '.join(sorted(cells))})"

        if reason:
            open(done, "w").close()  # once per window, even if the cycle fails
            log(f"{win}: {reason} -> starting hol_capped.sh")
            env = dict(os.environ, CELL_ONLY="1", WAIT_MIN="3")
            rc = subprocess.call([os.path.join(HERE, "hol_capped.sh")], env=env, cwd=DATA,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            log(f"{win}: hol_capped.sh exited {rc}")
            # then the AI agents with tools on the phone / in the cloud / split
            # (hol-base is the no-agent baseline); stops the same way when home
            env["PHASES"] = AGENT_PHASES
            log(f"{win}: starting app_test.sh {AGENT_PHASES}")
            rc = subprocess.call([os.path.join(HERE, "app_test.sh")], env=env, cwd=DATA,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            log(f"{win}: app_test.sh exited {rc}")
            return

        # keep the samples file small: drop lines older than a day
        if now.minute == 0 and now.second < SAMPLE_S:
            with open(samples_path) as f:
                keep = [l for l in f if t - int(l.split()[0]) < 86400]
            with open(samples_path, "w") as f:
                f.writelines(keep)
        time.sleep(SAMPLE_S)


if __name__ == "__main__":
    sys.exit(main())
