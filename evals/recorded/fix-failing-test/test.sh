. ./math.sh
result=$(add 2 3)
if [ "$result" = "5" ]; then
  echo "PASS"
else
  echo "FAIL: add 2 3 = $result, want 5"
  exit 1
fi
