#!/usr/bin/env bash
# GG8 network lab: a network namespace the daemon runs in, so that every packet it (or any child,
# such as llama-server) sends is visible on one veth pair and nowhere else.
#
#   netlab.sh up sinkhole   # no route to the internet; DNS answers every name with the sink
#   netlab.sh up internet   # NAT to the real internet; DNS is a logging forwarder
#   netlab.sh down
#
# Addresses (lab 0): the host side of the veth is 203.0.113.10 (TEST-NET-3, which Nugget classifies as
# `cloud`) and 192.168.88.1 (a private address, classified `local_network`); the namespace side
# is 203.0.113.2. Inside the namespace, /etc/resolv.conf points at 203.0.113.10 and
# /etc/nsswitch.conf resolves through files and DNS only (no systemd-resolved over D-Bus).
set -euo pipefail
# GG8_LAB=k (0-3) runs lab k beside the others: namespace gg8-k, 203.0.113.(64k+10)/26 and
# 192.168.(88+k).0/24. Without it: lab 0, namespace gg8.
LAB=${GG8_LAB:-0}
NS=gg8; [ "$LAB" != 0 ] && NS=gg8-$LAB
HOST_IF=gg8h$LAB
NS_IF=gg8n$LAB
SINK_IP=203.0.113.$((64*LAB+10))
LAN_IP=192.168.$((88+LAB)).1
NS_IP=203.0.113.$((64*LAB+2))
NS_LAN=192.168.$((88+LAB)).2

down() {
  ip netns del "$NS" 2>/dev/null || true
  ip link del "$HOST_IF" 2>/dev/null || true
  iptables -t nat -D POSTROUTING -s "$NS_IP/32" -j MASQUERADE 2>/dev/null || true
  iptables -D FORWARD -i "$HOST_IF" -j ACCEPT 2>/dev/null || true
  iptables -D FORWARD -o "$HOST_IF" -j ACCEPT 2>/dev/null || true
  rm -rf "/etc/netns/$NS"
}

up() {
  local mode=$1
  down
  ip netns add "$NS"
  ip link add "$HOST_IF" type veth peer name "$NS_IF"
  ip link set "$NS_IF" netns "$NS"
  ip addr add "$SINK_IP/26" dev "$HOST_IF"
  ip addr add "$LAN_IP/24" dev "$HOST_IF"
  ip link set "$HOST_IF" up
  ip netns exec "$NS" ip addr add "$NS_IP/26" dev "$NS_IF"
  ip netns exec "$NS" ip addr add "$NS_LAN/24" dev "$NS_IF"
  ip netns exec "$NS" ip link set lo up
  ip netns exec "$NS" ip link set "$NS_IF" up
  ip netns exec "$NS" ip route add default via "$SINK_IP"
  mkdir -p "/etc/netns/$NS"
  echo "nameserver $SINK_IP" > "/etc/netns/$NS/resolv.conf"
  printf 'passwd: files\ngroup: files\nshadow: files\nhosts: files dns\nnetworks: files\n' \
    > "/etc/netns/$NS/nsswitch.conf"
  printf '127.0.0.1 localhost\n::1 localhost ip6-localhost\n' > "/etc/netns/$NS/hosts"
  if [ "$mode" = internet ]; then
    sysctl -qw net.ipv4.ip_forward=1
    iptables -t nat -A POSTROUTING -s "$NS_IP/32" -j MASQUERADE
    iptables -I FORWARD -i "$HOST_IF" -j ACCEPT
    iptables -I FORWARD -o "$HOST_IF" -j ACCEPT
  fi
}

case "${1:-}" in
  up) up "${2:-sinkhole}" ;;
  down) down ;;
  *) echo "usage: $0 up sinkhole|internet | down" >&2; exit 2 ;;
esac
