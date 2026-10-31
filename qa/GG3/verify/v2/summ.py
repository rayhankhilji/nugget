import json,sys
d=json.load(open(sys.argv[1]))
for s in d['results']:
    print('##', s['name'], '|', s['what'], '| user', s['user'])
    for e in s['events']:
        if e.get('event') in ('confirmation_needed','action_denied','finished','suspended'):
            print('  EV', json.dumps({k:v for k,v in e.items() if k in('event','reason','risk','label','outcome','tool')},ensure_ascii=False)[:300])
    for t in s['transcript'][1:]:
        for sh in t['shown']:
            txt=sh.get('tool_result') or sh.get('text') or ''
            if 'untrusted' not in txt[:20] or len(sys.argv)>2: print('  SHOWN t%d:'%t['turn'], txt[:260].replace('\n',' | '))
    for l in s['legs']: print('  LEG', l['outcome'], [ (a['tool'],a['risk'],a['decision']) for a in l['actions']])
