#!/bin/sh
# A stand-in game server for the tests. It honours the launcher contract
# (DEVELOPMENT.md, "Launcher contract") and plays nothing:
#   fake_game.sh --probe <hex>                        prints the fingerprint
#   fake_game.sh <behaviour> <seconds> <match_id> <port> [more...]
# Its report is the hex of "<match_id>|<port>|$GAME_SEATS|<more>", so a test
# sees what the game was started with (<more>: the further arguments, space
# separated, e.g. a lobby's cheats).
set -eu
if [ "$1" = "--probe" ]; then
    echo "$2"
    exit 0
fi
behaviour=$1 delay=$2 match_id=$3 port=$4
shift 4
report=$(printf '%s|%s|%s|%s' "$match_id" "$port" "${GAME_SEATS:-}" "$*" | od -An -v -tx1 | tr -d ' \n')
case $behaviour in
    noready) exec sleep 3600 ;;
    fail) exit 1 ;;
esac
echo "an ignored line"
echo READY
case $behaviour in
    # Ends the match itself when told to stop: the service's request to finish.
    obedient)
        trap 'echo "RESULT winner=draw duration_ms=777 report=$report aborted=stopped v=3"; exit 0' TERM
        sleep 3600 &
        wait $!
        ;;
    # Answers the request to finish with a winner, as a game that had just finished does.
    winner_on_term)
        trap 'echo "RESULT winner=0 duration_ms=555 report=$report"; exit 0' TERM
        sleep 3600 &
        wait $!
        ;;
    # Claims a stop nobody asked for, or a limit that is not due.
    rogue_stop)
        echo "RESULT winner=draw duration_ms=1 report=$report aborted=stopped v=3"
        exit 0
        ;;
    rogue_limit)
        echo "RESULT winner=draw duration_ms=1 report=$report aborted=time_limit v=3"
        exit 0
        ;;
    # A game that was told its limit and ends the match itself with it: at the limit, or, if the
    # service asks before the game's own end (the tests' limits are tens of milliseconds, the game
    # waits the limit in seconds), as soon as it is asked. Either way the word is `time_limit`.
    limited)
        trap 'echo "RESULT winner=draw duration_ms=888 report=$report aborted=time_limit v=3"; exit 0' TERM
        sleep "${RADOVID_MAX_DURATION_SECS:?}" &
        wait $!
        echo "RESULT winner=draw duration_ms=888 report=$report aborted=time_limit v=3"
        exit 0
        ;;
esac
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
if [ -n "${GAME_META:-}" ]; then
    # Bytes the service has no way to understand.
    printf '\377\000meta of %s' "$match_id" > "$GAME_META"
fi
if [ -n "${GAME_ARTIFACT:-}" ]; then
    printf 'replay of %s' "$match_id" > "$GAME_ARTIFACT"
fi
case $behaviour in
    team0) echo "RESULT winner=0 duration_ms=1234 report=$report" ;;
    team1) echo "RESULT winner=1 duration_ms=1234 report=$report" ;;
    leave0) echo "RESULT winner=1 duration_ms=1234 report=$report left=0" ;;
    leave1) echo "RESULT winner=0 duration_ms=1234 report=$report left=1" ;;
    badleave) echo "RESULT winner=0 duration_ms=1234 report=$report left=0" ;;
    draw) echo "RESULT winner=draw duration_ms=1234 report=$report" ;;
    future) echo "RESULT winner=1 duration_ms=1234 report=$report replay=truncated later=a_field_of_a_newer_game" ;;
    futureversion) echo "RESULT winner=1 duration_ms=1234 report=$report v=4" ;;
    # A game prints the fields of a newer line only when told that radovid reads them.
    versioned) [ "${RADOVID_RESULT_VERSION:-1}" -ge 2 ] && echo "RESULT winner=0 duration_ms=1234 report=$report replay=truncated" || echo "RESULT winner=1 duration_ms=1234 report=$report" ;;
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
