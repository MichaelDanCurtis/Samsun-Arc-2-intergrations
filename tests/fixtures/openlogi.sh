#!/bin/sh
# Fake `openlogi` for tests. Env: FAKE_OPENLOGI_HERE=1 → keyboard online here on slot $FAKE_OPENLOGI_SLOT.
# `host switch 3` is refused (unpaired); `host switch <current>` is already current; others switch.
# Every switch appends its slot to $FAKE_OPENLOGI_LOG when set.
case "$1 $2" in
  "host list")
    if [ "$FAKE_OPENLOGI_HERE" = "1" ]; then
      printf '{"keyboards":[{"name":"FAKE KEYS","route":"direct 046d:b37c","online":true,"current_slot":%s,"slots":[]}]}\n' "${FAKE_OPENLOGI_SLOT:-1}"
    else
      printf '{"keyboards":[{"name":"FAKE KEYS","route":"direct 046d:b37c","online":false,"error":"offline"}]}\n'
    fi
    exit 0 ;;
  "host switch")
    [ -n "$FAKE_OPENLOGI_LOG" ] && printf '%s\n' "$3" >> "$FAKE_OPENLOGI_LOG"
    if [ "$FAKE_OPENLOGI_HERE" != "1" ]; then echo "no online keyboard matched" >&2; exit 3; fi
    if [ "$3" = "3" ]; then echo "the requested host slot is not paired on the keyboard" >&2; exit 2; fi
    if [ "$3" = "${FAKE_OPENLOGI_SLOT:-1}" ]; then echo "FAKE KEYS already on slot $3"; exit 0; fi
    echo "FAKE KEYS → slot $3"; exit 0 ;;
  *) echo "unknown command" >&2; exit 1 ;;
esac
