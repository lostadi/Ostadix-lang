#!/bin/sh
# Example external shell script invoked via run_script()
echo "Hello from an external shell script!"
echo "Current directory: $(pwd)"
echo "Date: $(date -u '+%Y-%m-%d')"
