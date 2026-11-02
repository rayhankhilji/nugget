export const FAQ: { q: string; a: string }[] = [
  {
    q: "Is there a free plan?",
    a: "No. Every plan starts with a 14-day free trial so you can see Heed working on your own mail and calendar. A card is required, and if you cancel before the trial ends you aren't charged. One trial per person.",
  },
  {
    q: "Does my data leave my Mac?",
    a: "Yes, and we'd rather be upfront about it. Heed is a cloud service: the apps you connect sync to your account in our London data centre, and when you ask a question or Heed writes for you, the relevant snippets are sent to our AI provider (Anthropic, or OpenAI as a fallback) to produce the answer. Heed doesn't run its AI on your device.",
  },
  {
    q: "Do you sell my data or use it to train AI?",
    a: "Never. We don't sell or share your data for advertising, and we don't train models on it. Our AI providers' API terms say they don't train on API data by default; they keep requests for a limited time for abuse monitoring.",
  },
  {
    q: "Which apps can I connect?",
    a: "Gmail and Google Calendar first, with Google Drive, Notion, Linear, GitHub and more on the way. Connecting takes one click: you approve access with the app itself, and Heed never sees your password.",
  },
  {
    q: "Where does ghost text work?",
    a: "In the text fields you already type in on your Mac: mail, chat, docs and the browser. Heed asks for macOS Accessibility permission so it can see where you're typing. Press Tab to accept a suggestion, ⌥→ to take one word, or just keep typing to ignore it.",
  },
  {
    q: "What is a deep answer?",
    a: "Some questions need more thought, like pulling a decision out of a long thread or comparing three documents. Deep answers use our most capable model. Heed includes 300 a month and uses the fast model after that; Heed Max uses the most capable model by default.",
  },
  {
    q: "Will Heed send anything without asking me?",
    a: "No. Heed reads and organises, and it drafts. Anything that leaves your account, like sending an email or creating an event, waits for you to approve it, and every action is logged.",
  },
  {
    q: "Can I use Heed on Windows or Linux?",
    a: "Not yet. Heed is macOS first (macOS 11 or later, Apple silicon and Intel). Windows and Linux are coming soon.",
  },
  {
    q: "How do I cancel or delete my account?",
    a: "Cancel any time from Manage billing on your account page; you keep access until the end of the period you paid for. Deleting your account disconnects every app and deletes all your synced data from our database.",
  },
];
