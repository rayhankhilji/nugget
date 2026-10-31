# time probe: corpus mtimes = item dates

wall clock (UTC): 2026-10-30T19:28:47.476611+00:00  
corpus: `/tmp/claude-0/gg2-time/corpus`, items indexed: 315

## Q01: 'Summarise last week'

expected range: ['2026-10-19', '2026-10-26']  
note: parser: last ISO week  
citations in expected range: 1/10

- [1] 2026-09-24 out `0:mail/9-newsletters/pantry-box-4.eml` 'Pantry Box: rate your last box'
- [2] 2026-10-20 IN  `0:mail/1-fernhill/resignation.eml` 'Resignation'
- [3] 2026-10-16 out `0:mail/2-corvid/03-accept.eml` 'Re: Your offer from Corvid Labs'
- [4] 2026-09-19 out `0:mail/2-corvid/01-offer-v1.eml` 'Your offer from Corvid Labs (draft v1)'
- [5] 2026-10-14 out `0:mail/5-bills/gym-cancel.eml` 'Membership cancellation confirmed'
- [6] 2026-09-25 out `0:mail/9-newsletters/the-weekly-byte-7.eml` 'The Weekly Byte: a guide to password managers'
- [7] 2026-07-13 out `0:mail/9-newsletters/pantry-box-3.eml` 'Pantry Box: skip a week reminder'
- [8] 2026-10-11 out `0:documents/work/Atlas vendor proposal.md` 'Atlas vendor proposal'
- [9] 2026-10-13 out `0:documents/work/Bob Lindqvist 1-1 notes.md` 'Bob Lindqvist 1-1 notes'
- [10] 2026-08-25 out `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'

## Q02: 'What emails did I get last week?'

expected range: ['2026-10-19', '2026-10-26']  
citations in expected range: 2/2

- [1] 2026-10-23 IN  `0:mail/2-corvid/06-welcome.eml` 'Welcome to Corvid!'
- [2] 2026-10-23 IN  `0:calendar/onboarding-call.ics` 'Corvid pre-boarding call with Lena'

## Q03: "What's happening this month?"

expected range: ['2026-10-01', '2026-11-01']  
citations in expected range: 3/3

- [1] 2026-10-20 IN  `0:mail/1-fernhill/resignation-reply.eml` 'Re: Resignation'
- [2] 2026-10-20 IN  `0:notes/book club.md` 'book club'
- [3] 2026-10-19 IN  `0:documents/work/Atlas steering notes.md` 'Atlas steering notes'

## Q04: 'What do I have next week?'

expected range: ['2026-11-02', '2026-11-09']  
note: user means events dated next week; parser has no future forms  
citations in expected range: 0/10

- [1] 2026-09-25 out `0:mail/9-newsletters/the-weekly-byte-7.eml` 'The Weekly Byte: a guide to password managers'
- [2] 2026-08-11 out `0:mail/9-newsletters/garden-notes-2.eml` 'Garden Notes: pruning roses'
- [3] 2026-08-25 out `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'
- [4] 2026-07-27 out `0:mail/9-newsletters/the-weekly-byte-4.eml` 'The Weekly Byte: battery breakthroughs'
- [5] 2026-08-09 out `0:mail/9-newsletters/city-arts-weekly-1.eml` 'City Arts Weekly: jazz at the Old Exchange'
- [6] 2026-10-25 out `0:mail/9-newsletters/the-weekly-byte-6.eml` 'The Weekly Byte: why every app is adding AI'
- [7] 2026-09-15 out `0:mail/9-newsletters/pantry-box-1.eml` 'Pantry Box: your box is on its way'
- [8] 2026-08-16 out `0:mail/9-newsletters/the-weekly-byte-1.eml` 'The Weekly Byte: chip shortages ease'
- [9] 2026-10-13 out `0:mail/6-travel/kyoto-flight.eml` 'Booking confirmed: Kyoto (Osaka Kansai)'
- [10] 2026-07-13 out `0:mail/9-newsletters/pantry-box-3.eml` 'Pantry Box: skip a week reminder'

## Q05: 'What do I have this week?'

expected range: ['2026-10-26', '2026-11-02']  
citations in expected range: 0/10

- [1] 2026-07-13 out `0:mail/9-newsletters/pantry-box-3.eml` 'Pantry Box: skip a week reminder'
- [2] 2026-09-25 out `0:mail/9-newsletters/the-weekly-byte-7.eml` 'The Weekly Byte: a guide to password managers'
- [3] 2026-08-11 out `0:mail/9-newsletters/garden-notes-2.eml` 'Garden Notes: pruning roses'
- [4] 2026-08-25 out `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'
- [5] 2026-07-27 out `0:mail/9-newsletters/the-weekly-byte-4.eml` 'The Weekly Byte: battery breakthroughs'
- [6] 2026-08-09 out `0:mail/9-newsletters/city-arts-weekly-1.eml` 'City Arts Weekly: jazz at the Old Exchange'
- [7] 2026-10-13 out `0:documents/work/Bob Lindqvist 1-1 notes.md` 'Bob Lindqvist 1-1 notes'
- [8] 2026-10-25 out `0:mail/9-newsletters/the-weekly-byte-6.eml` 'The Weekly Byte: why every app is adding AI'
- [9] 2026-09-15 out `0:mail/9-newsletters/pantry-box-1.eml` 'Pantry Box: your box is on its way'
- [10] 2026-10-14 out `0:mail/9-newsletters/pantry-box-2.eml` 'Pantry Box: new autumn recipes'

## Q06: 'What happened yesterday?'

expected range: ['2026-10-29', '2026-10-30']  
citations in expected range: 0/10

- [1] 2026-10-24 out `0:mail/1-fernhill/misc-07.eml` 'Standup notes'
- [2] 2026-08-26 out `0:mail/1-fernhill/misc-25.eml` 'Standup notes'
- [3] 2026-09-04 out `0:mail/1-fernhill/misc-12.eml` 'Standup notes'
- [4] 2026-09-21 out `0:mail/1-fernhill/misc-31.eml` 'Standup notes'
- [5] 2026-07-31 out `0:mail/1-fernhill/misc-32.eml` 'Standup notes'
- [6] 2026-08-07 out `0:mail/1-fernhill/misc-48.eml` 'Standup notes'
- [7] 2026-10-08 out `0:mail/1-fernhill/misc-60.eml` 'Standup notes'
- [8] 2026-08-24 out `0:mail/1-fernhill/misc-68.eml` 'Standup notes'
- [9] 2026-10-23 out `0:mail/1-fernhill/misc-70.eml` 'Standup notes'
- [10] 2026-08-31 out `0:mail/1-fernhill/misc-72.eml` 'Standup notes'

## Q07: 'What did I do in October?'

expected range: ['2026-10-01', '2026-11-01']  
citations in expected range: 10/10

- [1] 2026-10-24 IN  `0:chats/slack-sam-ortega.txt` 'slack-sam-ortega'
- [2] 2026-10-25 IN  `0:chats/whatsapp-jun.txt` 'whatsapp-jun'
- [3] 2026-10-20 IN  `0:mail/1-fernhill/resignation.eml` 'Resignation'
- [4] 2026-10-04 IN  `0:mail/1-fernhill/misc-52.eml` 'Interview loop'
- [5] 2026-10-07 IN  `0:mail/9-newsletters/riverside-running-club-5.eml` 'Riverside Running Club: track session moved to Tuesday'
- [6] 2026-10-16 IN  `0:mail/2-corvid/03-accept.eml` 'Re: Your offer from Corvid Labs'
- [7] 2026-10-23 IN  `0:mail/1-fernhill/farewell.eml` 'Farewell drinks'
- [8] 2026-10-24 IN  `0:mail/4-health/dentist-spoof.eml` 'Re: Your dentist appointment'
- [9] 2026-10-14 IN  `0:mail/9-newsletters/pantry-box-2.eml` 'Pantry Box: new autumn recipes'
- [10] 2026-10-07 IN  `0:mail/1-fernhill/budget-03.eml` 'Re: Q4 budget for Atlas'

## Q08: 'What do I have tomorrow?'

expected range: ['2026-10-31', '2026-11-01']  
note: not a supported phrase  
citations in expected range: 0/0


## Q09: 'What are my upcoming appointments?'

expected range: (none parsed / not a supported phrase)  
note: not a supported phrase  
citations in expected range: 0/4

- [1] 2026-09-24     `0:mail/4-health/dentist-booked.eml` 'Appointment confirmation'
- [2] 2026-10-24     `0:mail/4-health/dentist-spoof.eml` 'Re: Your dentist appointment'
- [3] 2026-10-23     `0:mail/4-health/blood-results.eml` 'Your blood test results'
- [4] 2026-10-21     `0:mail/4-health/dentist-rescheduled.eml` 'Your appointment has been rescheduled'

## Q10: 'When is my next dentist appointment?'

expected range: (none parsed / not a supported phrase)  
citations in expected range: 0/10

- [1] 2026-10-24     `0:mail/4-health/dentist-spoof.eml` 'Re: Your dentist appointment'
- [2] 2026-04-02     `0:calendar/dentist-last-time.ics` 'Dentist: check-up (Dr. Kim)'
- [3] 2026-09-25     `0:mail/9-newsletters/the-weekly-byte-7.eml` 'The Weekly Byte: a guide to password managers'
- [4] 2026-09-24     `0:mail/4-health/dentist-booked.eml` 'Appointment confirmation'
- [5] 2026-09-15     `0:mail/9-newsletters/pantry-box-1.eml` 'Pantry Box: your box is on its way'
- [6] 2026-08-25     `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'
- [7] 2026-08-16     `0:mail/9-newsletters/the-weekly-byte-1.eml` 'The Weekly Byte: chip shortages ease'
- [8] 2026-08-11     `0:mail/9-newsletters/garden-notes-2.eml` 'Garden Notes: pruning roses'
- [9] 2026-08-09     `0:mail/9-newsletters/city-arts-weekly-1.eml` 'City Arts Weekly: jazz at the Old Exchange'
- [10] 2026-10-21     `0:calendar/dentist.ics` 'Dentist: check-up and cleaning (Dr. Kim)'

## Q11: 'How long until my Kyoto trip?'

expected range: (none parsed / not a supported phrase)  
citations in expected range: 0/10

- [1] 2026-09-29     `0:notes/gift ideas.md` 'gift ideas'
- [2] 2026-10-14     `0:mail/5-bills/gym-cancel.eml` 'Membership cancellation confirmed'
- [3] 2026-08-25     `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'
- [4] 2026-07-06     `0:notes/misc/02 journal.md` '02 journal'
- [5] 2026-06-20     `0:notes/misc/04 journal.md` '04 journal'
- [6] 2026-07-05     `0:notes/misc/06 journal.md` '06 journal'
- [7] 2026-06-01     `0:notes/misc/11 journal.md` '11 journal'
- [8] 2026-08-21     `0:notes/misc/12 journal.md` '12 journal'
- [9] 2026-06-22     `0:notes/misc/13 journal.md` '13 journal'
- [10] 2026-10-10     `0:notes/misc/20 journal.md` '20 journal'

## Q12: 'Which bills are due soon?'

expected range: (none parsed / not a supported phrase)  
citations in expected range: 0/9

- [1] 2026-09-24     `0:mail/5-bills/citypower-prev.eml` 'Your City Power bill is ready'
- [2] 2026-10-24     `0:mail/5-bills/citypower.eml` 'Your City Power bill is ready'
- [3] 2026-07-16     `0:mail/5-bills/lumen-1.eml` 'Your Lumen Fiber bill for July'
- [4] 2026-05-24     `0:mail/6-travel/porto-conference.eml` 'Invoice: Hotel Ribeira Porto'
- [5] 2026-10-14     `0:mail/5-bills/gym-cancel.eml` 'Membership cancellation confirmed'
- [6] 2026-10-25     `0:mail/5-bills/lumen-phish.eml` 'Important: your Lumen Fiber account'
- [7] 2026-10-14     `0:mail/5-bills/lumen-4.eml` 'Your Lumen Fiber bill for October'
- [8] 2026-09-14     `0:mail/5-bills/lumen-3.eml` 'Your Lumen Fiber bill for September'
- [9] 2026-08-15     `0:mail/5-bills/lumen-2.eml` 'Your Lumen Fiber bill for August'

## Q13: 'What meetings did I have last week?'

expected range: ['2026-10-19', '2026-10-26']  
note: calendar: steering was Mon 19 Oct but its .ics mtime is 9 Oct  
citations in expected range: 2/2

- [1] 2026-10-20 IN  `0:notes/book club.md` 'book club'
- [2] 2026-10-23 IN  `0:calendar/onboarding-call.ics` 'Corvid pre-boarding call with Lena'

## Q14: 'What appointments do I have next week?'

expected range: ['2026-11-02', '2026-11-09']  
citations in expected range: 0/10

- [1] 2026-09-25 out `0:mail/9-newsletters/the-weekly-byte-7.eml` 'The Weekly Byte: a guide to password managers'
- [2] 2026-08-11 out `0:mail/9-newsletters/garden-notes-2.eml` 'Garden Notes: pruning roses'
- [3] 2026-08-25 out `0:mail/9-newsletters/riverside-running-club-1.eml` 'Riverside Running Club: Sunday long run route'
- [4] 2026-07-27 out `0:mail/9-newsletters/the-weekly-byte-4.eml` 'The Weekly Byte: battery breakthroughs'
- [5] 2026-08-09 out `0:mail/9-newsletters/city-arts-weekly-1.eml` 'City Arts Weekly: jazz at the Old Exchange'
- [6] 2026-10-25 out `0:mail/9-newsletters/the-weekly-byte-6.eml` 'The Weekly Byte: why every app is adding AI'
- [7] 2026-09-15 out `0:mail/9-newsletters/pantry-box-1.eml` 'Pantry Box: your box is on its way'
- [8] 2026-09-24 out `0:mail/4-health/dentist-booked.eml` 'Appointment confirmation'
- [9] 2026-10-24 out `0:mail/4-health/dentist-spoof.eml` 'Re: Your dentist appointment'
- [10] 2026-08-16 out `0:mail/9-newsletters/the-weekly-byte-1.eml` 'The Weekly Byte: chip shortages ease'

## Q15: 'What events were added last week?'

expected range: ['2026-10-19', '2026-10-26']  
note: future events whose .ics was written last week  
citations in expected range: 4/4

- [1] 2026-10-25 IN  `0:mail/9-newsletters/the-weekly-byte-6.eml` 'The Weekly Byte: why every app is adding AI'
- [2] 2026-10-20 IN  `0:calendar/misc/42-team.ics` 'Team standup'
- [3] 2026-10-21 IN  `0:calendar/dentist.ics` 'Dentist: check-up and cleaning (Dr. Kim)'
- [4] 2026-10-23 IN  `0:calendar/onboarding-call.ics` 'Corvid pre-boarding call with Lena'

## Q16: 'dentist last week'

expected range: ['2026-10-19', '2026-10-26']  
note: next dentist appt is 5 Nov, its .ics mtime is 21 Oct  
citations in expected range: 2/2

- [1] 2026-10-21 IN  `0:calendar/dentist.ics` 'Dentist: check-up and cleaning (Dr. Kim)'
- [2] 2026-10-24 IN  `0:mail/4-health/dentist-spoof.eml` 'Re: Your dentist appointment'

## Q17: "What's on my calendar on Thursday 5 November 2026?"

expected range: ['2026-11-05', '2026-11-06']  
note: long-form date: not a supported phrase  
citations in expected range: 0/10

- [1] 2026-10-22 out `0:documents/work/Handover plan.md` 'Handover plan'
- [2] 2026-10-21 out `0:mail/4-health/dentist-rescheduled.eml` 'Your appointment has been rescheduled'
- [3] 2026-10-23 out `0:mail/1-fernhill/farewell.eml` 'Farewell drinks'
- [4] 2026-10-20 out `0:mail/1-fernhill/resignation-reply.eml` 'Re: Resignation'
- [5] 2026-10-24 out `0:mail/5-bills/citypower.eml` 'Your City Power bill is ready'
- [6] 2026-10-19 out `0:documents/work/Atlas steering notes.md` 'Atlas steering notes'
- [7] 2026-10-22 out `0:documents/contracts/Corvid offer letter v3 FINAL signed.md` 'Corvid offer letter v3 FINAL signed'
- [8] 2026-07-16 out `0:mail/1-fernhill/misc-29.eml` 'Interview loop'
- [9] 2026-10-20 out `0:notes/book club.md` 'book club'
- [10] 2026-07-31 out `0:chats/team-product-1.txt` 'team-product-1'

## Q18: "What's on my calendar on 2026-11-05?"

expected range: ['2026-11-05', '2026-11-06']  
note: ISO date is supported; future day  
citations in expected range: 0/10

- [1] 2026-10-21 out `0:calendar/dentist.ics` 'Dentist: check-up and cleaning (Dr. Kim)'
- [2] 2026-08-29 out `0:calendar/misc/26-team.ics` 'Team standup'
- [3] 2026-07-11 out `0:mail/6-travel/lisbon-flight.eml` 'Booking confirmed: Lisbon'
- [4] 2026-10-20 out `0:mail/1-fernhill/resignation-reply.eml` 'Re: Resignation'
- [5] 2026-10-24 out `0:chats/slack-sam-ortega.txt` 'slack-sam-ortega'
- [6] 2026-11-01 out `0:calendar/misc/23-quarry.ics` 'Quarry roadmap sync'
- [7] 2026-04-02 out `0:calendar/dentist-last-time.ics` 'Dentist: check-up (Dr. Kim)'
- [8] 2026-10-20 out `0:notes/book club.md` 'book club'
- [9] 2026-10-28 out `0:calendar/misc/17-focus.ics` 'Focus time'
- [10] 2026-09-19 out `0:chats/team-product-6.txt` 'team-product-6'

## Q19: 'What did I get in the past week?'

expected range: ['2026-10-23', '2026-10-31']  
citations in expected range: 1/1

- [1] 2026-10-23 IN  `0:mail/2-corvid/06-welcome.eml` 'Welcome to Corvid!'

## Q20: 'Any emails from yesterday?'

expected range: ['2026-10-29', '2026-10-30']  
citations in expected range: 0/10

- [1] 2026-10-24 out `0:mail/1-fernhill/misc-07.eml` 'Standup notes'
- [2] 2026-10-23 out `0:calendar/onboarding-call.ics` 'Corvid pre-boarding call with Lena'
- [3] 2026-08-26 out `0:mail/1-fernhill/misc-25.eml` 'Standup notes'
- [4] 2026-09-04 out `0:mail/1-fernhill/misc-12.eml` 'Standup notes'
- [5] 2026-09-21 out `0:mail/1-fernhill/misc-31.eml` 'Standup notes'
- [6] 2026-07-31 out `0:mail/1-fernhill/misc-32.eml` 'Standup notes'
- [7] 2026-08-07 out `0:mail/1-fernhill/misc-48.eml` 'Standup notes'
- [8] 2026-10-08 out `0:mail/1-fernhill/misc-60.eml` 'Standup notes'
- [9] 2026-08-24 out `0:mail/1-fernhill/misc-68.eml` 'Standup notes'
- [10] 2026-10-23 out `0:mail/1-fernhill/misc-70.eml` 'Standup notes'

