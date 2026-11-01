English/German twin mails for GG7-4/GG7-5 (proactive). Run with
`python3 qa/GG7/harness/gg7.py run --nuggetd target/debug/nuggetd --home /tmp/claude-0/tw --out /tmp/claude-0/tw/out --corpus qa/GG7/scenarios/twins --no-models --suites proactive --langs en`
and read `cards.list` in out/proactive.jsonl: the en-* mails give `unanswered_thread`, `follow_up` and `renewal` cards, the de-* twins none.
