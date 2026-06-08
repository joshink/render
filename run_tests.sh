#!/bin/bash
set -e

# Define colors for output
GREEN='\033[0;32m'
NC='\033[0m' # No Color
BOLD='\033[1m'

echo -e "${BOLD}====================================================${NC}"
echo -e "${BOLD} 1. Building Render PoC Engine...${NC}"
echo -e "${BOLD}====================================================${NC}"
cargo build --release

echo ""
echo -e "${BOLD}====================================================${NC}"
echo -e "${BOLD} 2. Running Progressive Integration Tests...${NC}"
echo -e "${BOLD}====================================================${NC}"
cargo test --test integration --release

echo ""
echo -e "${BOLD}====================================================${NC}"
echo -e "${BOLD} 3. Generated Output Files:${NC}"
echo -e "${BOLD}====================================================${NC}"
ls -la test_cases/outputs/

echo ""
echo -e "${GREEN}${BOLD}✓ All progressive test cases ran and verified successfully!${NC}"
echo -e "${BOLD}====================================================${NC}"
