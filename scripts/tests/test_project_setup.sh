#!/usr/bin/env bash

# Test script for Diapason project management

echo "Testing Diapason project management..."

if [ -n "${XDG_STATE_HOME:-}" ]; then
    case "$XDG_STATE_HOME" in
        /*) STATE_DIR="$XDG_STATE_HOME/diapason" ;;
        *) STATE_DIR="$HOME/.local/state/diapason" ;;
    esac
else
    STATE_DIR="$HOME/.local/state/diapason"
fi
PROJECTS_DIR="$STATE_DIR/projects"

# Check if the canonical state projects directory exists
if [ -d "$PROJECTS_DIR" ]; then
    echo "✓ Found $PROJECTS_DIR"
else
    echo "✗ $PROJECTS_DIR not found"
    exit 1
fi

# Test creating a sample project structure
echo "Creating test project structure..."
mkdir -p "$PROJECTS_DIR/test-project/config"
mkdir -p "$PROJECTS_DIR/test-project/cache"
mkdir -p "$PROJECTS_DIR/test-project/embeddings"
mkdir -p "$PROJECTS_DIR/test-project/retrieval"

# Check if all directories were created
if [ -d "$PROJECTS_DIR/test-project/config" ]; then
    echo "✓ Config directory created"
else
    echo "✗ Config directory not created"
fi

if [ -d "$PROJECTS_DIR/test-project/cache" ]; then
    echo "✓ Cache directory created"
else
    echo "✗ Cache directory not created"
fi

if [ -d "$PROJECTS_DIR/test-project/embeddings" ]; then
    echo "✓ Embeddings directory created"
else
    echo "✗ Embeddings directory not created"
fi

if [ -d "$PROJECTS_DIR/test-project/retrieval" ]; then
    echo "✓ Retrieval directory created"
else
    echo "✗ Retrieval directory not created"
fi

# Create a simple .project metadata file
cat > "$PROJECTS_DIR/test-project/.project" << EOF
{
  "name": "test-project",
  "description": "Test project for Diapason",
  "created_at": $(date +%s),
  "updated_at": $(date +%s),
  "root_path": "/tmp/test-project",
  "tags": ["test", "diapason"]
}
EOF

if [ -f "$PROJECTS_DIR/test-project/.project" ]; then
    echo "✓ Project metadata file created"
else
    echo "✗ Project metadata file not created"
fi

echo "Test completed."
