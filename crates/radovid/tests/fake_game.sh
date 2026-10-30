#!/bin/sh
# A stand-in game server for the tests. It honours the launcher contract
# (README, "Launcher contract") and plays nothing:
#   fake_game.sh --probe <hex>                        prints the fingerprint
#   fake_game.sh <behaviour> <seconds> <match_id> <port>
# Its report is the hex of "<match_id>|<port>|$GAME_SEATS", so a test sees
# what the game was started with.
set -eu
if [ "$1" = "--probe" ]; then
    echo "$2"
    exit 0
fi
behaviour=$1 delay=$2 match_id=$3 port=$4
report=$(printf '%s|%s|%s' "$match_id" "$port" "${GAME_SEATS:-}" | od -An -v -tx1 | tr -d ' \n')
case $behaviour in
    noready) exec sleep 3600 ;;
    fail) exit 1 ;;
esac
echo "an ignored line"
echo READY
sleep "$delay"
# A game told where to leave its replay does.
if [ "$behaviour" = slowfile ]; then
    # The result first, then the replay in two parts with a pause between, then the exit.
    printf 'replay of %s' "$match_id" > "$GAME_ARTIFACT"
    echo "RESULT winner=0 duration_ms=1234 report=$report"
    sleep 2
    printf ' and its end' >> "$GAME_ARTIFACT"
    exit 0
fi
if [ -n "${GAME_ARTIFACT:-}" ]; then
    printf 'replay of %s' "$match_id" > "$GAME_ARTIFACT"
fi
case $behaviour in
    team0) echo "RESULT winner=0 duration_ms=1234 report=$report" ;;
    team1) echo "RESULT winner=1 duration_ms=1234 report=$report" ;;
    leave0) echo "RESULT winner=1 duration_ms=1234 report=$report left=0" ;;
    badleave) echo "RESULT winner=0 duration_ms=1234 report=$report left=0" ;;
    draw) echo "RESULT winner=draw duration_ms=1234 report=$report" ;;
    badteam) echo "RESULT winner=5 duration_ms=1234 report=$report" ;;
    noisy)
        printf 'a line that is no UTF-8: \377\376\n' >&2
        i=0
        while [ "$i" -lt 1000 ]; do
            echo "stderr line $i" >&2
            i=$((i + 1))
        done
        echo "RESULT winner=0 duration_ms=1234 report=$report"
        ;;
    crash) exit 1 ;;
    abandon) exit 3 ;;
    hang) exec sleep 3600 ;;
esac
