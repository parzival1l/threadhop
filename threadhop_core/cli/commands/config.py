"""``threadhop config`` — read or update app-level config values."""

from __future__ import annotations

import sys

from ...config.loader import (
    CONFIG_FILE,
    _coerce_config_value,
    _config_value_to_text,
    _load_app_config_file,
    save_app_config,
)


def cmd_config(args) -> int:
    """Read or update app-level config.json values."""
    if args.config_command == "get":
        config = _load_app_config_file()
        if args.key == "observe.enabled":
            value = config.get(args.key, False)
        else:
            if args.key not in config:
                print(
                    f"threadhop config: {args.key} is not set.",
                    file=sys.stderr,
                )
                return 1
            value = config[args.key]
        print(_config_value_to_text(value))
        return 0

    if args.config_command == "set":
        try:
            value = _coerce_config_value(args.key, args.value)
        except ValueError as e:
            print(f"threadhop config: {e}", file=sys.stderr)
            return 2
        config = _load_app_config_file()
        config[args.key] = value
        if not save_app_config(config):
            print(
                f"threadhop config: could not write {CONFIG_FILE}.",
                file=sys.stderr,
            )
            return 1
        print(f"{args.key} = {_config_value_to_text(value)}")
        return 0

    print(
        f"threadhop config: unsupported action {args.config_command}",
        file=sys.stderr,
    )
    return 2
