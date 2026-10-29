#!/bin/sh
# Prints what macOS privacy (TCC) grants on this machine, for the nugget-a11y AX test: the
# Accessibility, PostEvent, ListenEvent and ScreenCapture rows of the system and user TCC
# databases, and the process chain of this shell (TCC judges the "responsible" process).
# Diagnostic only: never fails.
set -u
echo "== macOS"
sw_vers 2>&1
echo "== process chain of this shell"
pid=$$
i=0
while [ "$pid" -gt 1 ] && [ $i -lt 15 ]; do
  ps -o pid=,ppid=,user=,command= -p "$pid" 2>&1
  pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
  [ -n "$pid" ] || break
  i=$((i + 1))
done
q="select service, client, client_type, auth_value, auth_reason, datetime(last_modified,'unixepoch') from access where service in ('kTCCServiceAccessibility','kTCCServicePostEvent','kTCCServiceListenEvent','kTCCServiceScreenCapture','kTCCServiceAppleEvents','kTCCServiceSystemPolicyAllFiles') order by service, client"
for db in "/Library/Application Support/com.apple.TCC/TCC.db" "$HOME/Library/Application Support/com.apple.TCC/TCC.db"; do
  echo "== $db (service | client | client_type | auth_value 2=allowed | auth_reason | modified)"
  if [ -r "$db" ]; then
    sqlite3 -separator ' | ' "$db" "$q" 2>&1
  else
    sudo -n sqlite3 -separator ' | ' "$db" "$q" 2>&1
  fi
done
exit 0
