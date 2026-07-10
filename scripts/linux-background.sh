#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTION="${1:-}"
SERVICE="${2:-runner}"

case "$SERVICE" in
  runner)
    BIN_NAME="gitlab-work-runner"
    ;;
  dashboard)
    BIN_NAME="gitlab-work-runner-dashboard"
    ;;
  *)
    echo "unknown service: $SERVICE" >&2
    echo "usage: $0 {start|stop|restart|status} [runner|dashboard]" >&2
    exit 2
    ;;
esac

BIN_PATH="${BIN_PATH:-$ROOT_DIR/target/release/$BIN_NAME}"
PID_DIR="${PID_DIR:-$ROOT_DIR/run}"
LOG_DIR="${LOG_DIR:-$ROOT_DIR/logs}"
PID_FILE="${PID_FILE:-$PID_DIR/$BIN_NAME.pid}"
OUT_FILE="${OUT_FILE:-$LOG_DIR/$BIN_NAME.out}"

usage() {
  echo "usage: $0 {start|stop|restart|status} [runner|dashboard]"
  echo
  echo "environment overrides:"
  echo "  BIN_PATH=/path/to/$BIN_NAME"
  echo "  PID_DIR=/path/to/run"
  echo "  LOG_DIR=/path/to/logs"
  echo "  PID_FILE=/path/to/$BIN_NAME.pid"
  echo "  OUT_FILE=/path/to/$BIN_NAME.out"
}

pid_running() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

read_pid() {
  if [[ -f "$PID_FILE" ]]; then
    tr -d '[:space:]' < "$PID_FILE"
  fi
}

start_service() {
  if [[ ! -x "$BIN_PATH" ]]; then
    echo "binary is not executable: $BIN_PATH" >&2
    echo "build it first: cargo build --release" >&2
    exit 1
  fi

  mkdir -p "$PID_DIR" "$LOG_DIR"

  local existing_pid
  existing_pid="$(read_pid || true)"
  if pid_running "$existing_pid"; then
    echo "$BIN_NAME is already running, pid=$existing_pid"
    return
  fi

  rm -f "$PID_FILE"
  cd "$ROOT_DIR"
  nohup "$BIN_PATH" >> "$OUT_FILE" 2>&1 &
  local pid="$!"
  echo "$pid" > "$PID_FILE"
  sleep 1

  if pid_running "$pid"; then
    echo "started $BIN_NAME, pid=$pid"
    echo "stdout/stderr: $OUT_FILE"
  else
    rm -f "$PID_FILE"
    echo "failed to start $BIN_NAME; see $OUT_FILE" >&2
    exit 1
  fi
}

stop_service() {
  local pid
  pid="$(read_pid || true)"
  if ! pid_running "$pid"; then
    rm -f "$PID_FILE"
    echo "$BIN_NAME is not running"
    return
  fi

  kill "$pid"
  for _ in {1..30}; do
    if ! pid_running "$pid"; then
      rm -f "$PID_FILE"
      echo "stopped $BIN_NAME"
      return
    fi
    sleep 1
  done

  echo "$BIN_NAME did not stop after 30s; sending SIGKILL" >&2
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$PID_FILE"
}

status_service() {
  local pid
  pid="$(read_pid || true)"
  if pid_running "$pid"; then
    echo "$BIN_NAME is running, pid=$pid"
  else
    echo "$BIN_NAME is not running"
    exit 3
  fi
}

case "$ACTION" in
  start)
    start_service
    ;;
  stop)
    stop_service
    ;;
  restart)
    stop_service
    start_service
    ;;
  status)
    status_service
    ;;
  *)
    usage
    exit 2
    ;;
esac
