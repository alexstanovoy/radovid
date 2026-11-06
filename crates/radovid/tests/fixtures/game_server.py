#!/usr/bin/env python3
"""Development-only launcher fixture. No game traffic passes through radovid.

Three tiny protocols share seat admission: tic-tac-toe, a clock-controlled turn
counter, and a fixed-tick grid race. Everything binds to loopback. The race ends
when each seat has made eight moves; network scheduling cannot choose its winner.
"""
import argparse
import hmac
import os
from pathlib import Path
import selectors
import socket
import time

FINGERPRINTS = {"ttt": "7474740000000001", "clock": "636c6f636b000001", "grid": "6772696400000001"}


def arguments():
    parser = argparse.ArgumentParser()
    parser.add_argument("--game", choices=FINGERPRINTS, required=True)
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--clock-ms", type=int, default=200)
    options = parser.parse_args()
    if options.probe:
        print(FINGERPRINTS[options.game], flush=True)
        raise SystemExit(0)
    assert 1 <= options.clock_ms <= 60000
    return options


class Game:
    def __init__(self, options):
        self.options = options
        self.seats = [seat.split(":") for seat in os.environ["GAME_SEATS"].split(",")]
        assert 2 <= len(self.seats) <= 10
        assert all(len(seat) == 4 and len(seat[0]) == 64 for seat in self.seats)
        self.selector = selectors.DefaultSelector()
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", options.port))
        self.listener.listen(20)
        self.listener.setblocking(False)
        self.selector.register(self.listener, selectors.EVENT_READ)
        self.peers = {}
        self.connected = {}
        self.visits = [0] * len(self.seats)
        self.moves = [0] * len(self.seats)
        self.pending = [[] for _ in self.seats]
        self.positions = [0] * len(self.seats)
        self.board = ["."] * 9
        self.turn = 0
        self.started = False
        self.tick = 0
        self.remaining = [options.clock_ms / 1000] * len(self.seats)
        self.clock_at = time.monotonic()
        self.next_tick = time.monotonic()
        self.outcome = None

    def send(self, peer, text):
        # Tiny bounded responses fit a local socket buffer; a slow client is
        # removed instead of stalling the tick loop.
        try:
            peer.sendall((text + "\n").encode("ascii"))
        except OSError:
            self.close(peer)

    def close(self, peer):
        entry = self.peers.pop(peer, None)
        if entry is None:
            return
        if entry[1] is not None:
            self.connected.pop(entry[1], None)
        self.selector.unregister(peer)
        peer.close()

    def admit(self, peer, token):
        slot = next((i for i, seat in enumerate(self.seats)
                     if hmac.compare_digest(seat[0], token)), None)
        if slot is None or slot in self.connected:
            self.send(peer, "REFUSED")
            self.close(peer)
            return
        self.peers[peer][1] = slot
        self.connected[slot] = peer
        self.visits[slot] += 1
        self.send(peer, f"SEAT {slot} {self.seats[slot][1]}")
        if not self.started and len(self.connected) == len(self.seats):
            self.started = True
            self.clock_at = time.monotonic()
            self.next_tick = self.clock_at + 0.01

    def command(self, peer, line):
        slot = self.peers[peer][1]
        if slot is None:
            self.admit(peer, line)
        elif line == "DROP":
            self.send(peer, "BYE")
            self.close(peer)
        elif not self.started:
            self.send(peer, "WAIT")
        elif self.options.game == "grid":
            if line not in ("INPUT 1", "INPUT 2"):
                self.send(peer, "INVALID")
            elif self.moves[slot] + len(self.pending[slot]) < 8:
                self.pending[slot].append(int(line[-1]))
        else:
            self.move(peer, slot, line)

    def move(self, peer, slot, line):
        if slot != self.turn or len(line) != 6 or not line.startswith("MOVE ") or line[-1] not in "012345678":
            self.send(peer, "INVALID")
            return
        cell = int(line[-1])
        if self.board[cell] != ".":
            self.send(peer, "INVALID")
            return
        self.board[cell] = str(slot)
        self.moves[slot] += 1
        self.send(peer, "OK")
        lines = ((0, 1, 2), (3, 4, 5), (6, 7, 8), (0, 3, 6), (1, 4, 7), (2, 5, 8), (0, 4, 8), (2, 4, 6))
        if any(all(self.board[cell] == str(slot) for cell in line) for line in lines):
            self.outcome = self.seats[slot][1]
        elif "." not in self.board:
            self.outcome = "draw"
        self.turn = 1 - self.turn

    def receive(self, peer):
        try:
            chunk = peer.recv(512)
        except BlockingIOError:
            return
        if not chunk:
            self.close(peer)
            return
        entry = self.peers[peer]
        entry[0] += chunk
        if len(entry[0]) > 512:
            self.close(peer)
            return
        for _ in range(32):
            if peer not in self.peers or b"\n" not in entry[0] or self.outcome is not None:
                break
            line, entry[0] = entry[0].split(b"\n", 1)
            try:
                self.command(peer, line.decode("ascii"))
            except UnicodeError:
                self.close(peer)
                break

    def advance(self):
        if not self.started:
            return
        now = time.monotonic()
        if self.options.game == "clock":
            self.remaining[self.turn] -= now - self.clock_at
            self.clock_at = now
            if self.remaining[self.turn] <= 0:
                self.outcome = self.seats[1 - self.turn][1]
        if self.options.game == "grid" and now >= self.next_tick:
            self.tick += 1
            self.next_tick += 0.01
            for slot, moves in enumerate(self.pending):
                self.positions[slot] += sum(moves)
                self.moves[slot] += len(moves)
                moves.clear()
            for peer in list(self.connected.values()):
                self.send(peer, f"TICK {self.tick} {','.join(map(str, self.positions))}")
            if min(self.moves) >= 8 and self.visits[0] >= 2:
                scores = {}
                for slot, seat in enumerate(self.seats):
                    team = seat[1]
                    scores[team] = scores.get(team, 0) + self.positions[slot]
                best = max(scores.values())
                winners = [team for team, score in scores.items() if score == best]
                self.outcome = winners[0] if len(winners) == 1 else "draw"

    def run(self):
        print("READY", flush=True)
        deadline = time.monotonic() + 120
        for _ in range(200000):
            if time.monotonic() > deadline:
                raise RuntimeError("fixture deadline")
            self.advance()
            if self.outcome is not None:
                break
            for key, _ in self.selector.select(0.01):
                if key.fileobj is self.listener:
                    peer, _ = self.listener.accept()
                    peer.setblocking(False)
                    if len(self.peers) >= 20:
                        peer.close()
                    else:
                        self.peers[peer] = [b"", None]
                        self.selector.register(peer, selectors.EVENT_READ)
                else:
                    self.receive(key.fileobj)
        assert self.outcome is not None, "fixture operation budget"
        report = (f"game={self.options.game};clock_ms={self.options.clock_ms};"
                  f"moves={sum(self.moves)};reconnects={sum(max(0, n - 1) for n in self.visits)};"
                  f"ticks={self.tick};distances={','.join(map(str, self.positions))};pid={os.getpid()};board={''.join(self.board)}").encode()
        if "GAME_RECORD" in os.environ:
            Path(os.environ["GAME_RECORD"]).write_bytes(report)
        for peer in list(self.connected.values()):
            self.send(peer, f"END {self.outcome}")
        print(f"RESULT winner={self.outcome} duration_ms={self.tick * 10} report={report.hex()}", flush=True)
        for peer in list(self.peers):
            self.close(peer)
        self.listener.close()
        self.selector.close()


if __name__ == "__main__":
    Game(arguments()).run()
