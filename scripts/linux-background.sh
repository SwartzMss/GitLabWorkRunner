#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTION="${1:-}"
SERVICE="${2:-all}"
PID_DIR="${PID_DIR:-$ROOT_DIR/run}"
LOG_DIR="${LOG_DIR:-$ROOT_DIR/logs}"

usage() {
  echo "usage: $0 {start|stop|restart|status} [all|runner|dashboard]"
  echo
  echo "default service is all, which manages runner and dashboard together."
  echo "by default, binaries are resolved from the release package root first,"
  echo "then from target/release for source-tree development."
  echo "environment overrides:"
  echo "  RUNNER_BIN=/path/to/gitlab-work-runner"
  echo "  DASHBOARD_BIN=/path/to/gitlab-work-runner-dashboard"
  echo "  PID_DIR=/path/to/run"
  echo "  LOG_DIR=/path/to/logs"
}

service_bin_name() {
  case "$1" in
    runner)
      echo "gitlab-work-runner"
      ;;
    dashboard)
      echo "gitlab-work-runner-dashboard"
      ;;
    *)
      echo "unknown service: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
}

service_bin_path() {
  case "$1" in
    runner)
      if [[ -n "${RUNNER_BIN:-}" ]]; then
        echo "$RUNNER_BIN"
      elif [[ -x "$ROOT_DIR/gitlab-work-runner" ]]; then
        echo "$ROOT_DIR/gitlab-work-runner"
      else
        echo "$ROOT_DIR/target/release/gitlab-work-runner"
      fi
      ;;
    dashboard)
      if [[ -n "${DASHBOARD_BIN:-}" ]]; then
        echo "$DASHBOARD_BIN"
      elif [[ -x "$ROOT_DIR/gitlab-work-runner-dashboard" ]]; then
        echo "$ROOT_DIR/gitlab-work-runner-dashboard"
      else
        echo "$ROOT_DIR/target/release/gitlab-work-runner-dashboard"
      fi
      ;;
  esac
}

service_pid_file() {
  echo "$PID_DIR/$(service_bin_name "$1").pid"
}

service_out_file() {
  echo "$LOG_DIR/$(service_bin_name "$1").out"
}

pid_running() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

read_pid() {
  local pid_file="$1"
  if [[ -f "$pid_file" ]]; then
    tr -d '[:space:]' < "$pid_file"
  fi
}

start_service() {
  local service="$1"
  local bin_name bin_path pid_file out_file
  bin_name="$(service_bin_name "$service")"
  bin_path="$(service_bin_path "$service")"
  pid_file="$(service_pid_file "$service")"
  out_file="$(service_out_file "$service")"

  if [[ ! -x "$bin_path" ]]; then
    echo "binary is not executable: $bin_path" >&2
    echo "build it first with: cargo build --release" >&2
    echo "or set RUNNER_BIN/DASHBOARD_BIN to an executable path." >&2
    exit 1
  fi

  mkdir -p "$PID_DIR" "$LOG_DIR"

  local existing_pid
  existing_pid="$(read_pid "$pid_file" || true)"
  if pid_running "$existing_pid"; then
    echo "$bin_name is already running, pid=$existing_pid"
    return
  fi

  rm -f "$pid_file"
  cd "$ROOT_DIR"
  nohup "$bin_path" >> "$out_file" 2>&1 &
  local pid="$!"
  echo "$pid" > "$pid_file"
  sleep 1

  if pid_running "$pid"; then
    echo "started $bin_name, pid=$pid"
    echo "stdout/stderr: $out_file"
  else
    rm -f "$pid_file"
    echo "failed to start $bin_name; see $out_file" >&2
    exit 1
  fi
}

stop_service() {
  local service="$1"
  local bin_name pid_file pid
  bin_name="$(service_bin_name "$service")"
  pid_file="$(service_pid_file "$service")"
  pid="$(read_pid "$pid_file" || true)"
  if ! pid_running "$pid"; then
    rm -f "$pid_file"
    echo "$bin_name is not running"
    return
  fi

  kill "$pid"
  for _ in {1..30}; do
    if ! pid_running "$pid"; then
      rm -f "$pid_file"
      echo "stopped $bin_name"
      return
    fi
    sleep 1
  done

  echo "$bin_name did not stop after 30s; sending SIGKILL" >&2
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$pid_file"
}

status_service() {
  local service="$1"
  local bin_name pid_file pid
  bin_name="$(service_bin_name "$service")"
  pid_file="$(service_pid_file "$service")"
  pid="$(read_pid "$pid_file" || true)"
  if pid_running "$pid"; then
    echo "$bin_name is running, pid=$pid"
  else
    echo "$bin_name is not running"
    return 3
  fi
}

for_selected_services() {
  local fn="$1"
  case "$SERVICE" in
    all)
      "$fn" runner
      "$fn" dashboard
      ;;
    runner|dashboard)
      "$fn" "$SERVICE"
      ;;
    *)
      echo "unknown service: $SERVICE" >&2
      usage >&2
      exit 2
      ;;
  esac
}

case "$ACTION" in
  start)
    for_selected_services start_service
    ;;
  stop)
    for_selected_services stop_service
    ;;
  restart)
    for_selected_services stop_service
    for_selected_services start_service
    ;;
  status)
    status_code=0
    for_selected_services status_service || status_code=$?
    exit "$status_code"
    ;;
  *)
    usage
    exit 2
    ;;
esac
