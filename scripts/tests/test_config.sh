#!/usr/bin/env bash

# Test script for Diapason config loading from the canonical user directory

echo "Testing Diapason configuration loading..."

CONFIG_DIR=""
case "${DIAPASON_CONFIG:-}" in
    /*) CONFIG_DIR="$DIAPASON_CONFIG" ;;
esac
if [ -z "$CONFIG_DIR" ]; then
    case "${XDG_CONFIG_HOME:-}" in
        /*) CONFIG_DIR="$XDG_CONFIG_HOME/diapason" ;;
        *) CONFIG_DIR="$HOME/.config/diapason" ;;
    esac
fi

# Check if the canonical config directory exists
if [ -d "$CONFIG_DIR" ]; then
    echo "✓ Found $CONFIG_DIR"

    if [ -f "$CONFIG_DIR/config.toml" ]; then
        echo "✓ Found $CONFIG_DIR/config.toml"

        # Check if the file has content
        if [ -s "$CONFIG_DIR/config.toml" ]; then
            echo "✓ Configuration file has content"
        else
            echo "⚠ Configuration file is empty"
        fi
    else
        echo "✗ $CONFIG_DIR/config.toml not found"
    fi
else
    echo "✗ $CONFIG_DIR not found"
fi

echo "Test completed."
