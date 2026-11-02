import { appList } from "./apps";

export const FAQ: { q: string; a: string; link?: { href: string; text: string } }[] = [
  {
    q: "Is Heed free?",
    a: "The Heed app is free and open source under Apache-2.0. You run it on your Mac, bring your own AI and connect your apps yourself. Heed Cloud is the paid option: one-click connections, always-on sync, AI included and support. Both cloud plans start with a 14-day free trial; a card is needed, and you can cancel any time before you're charged. One trial per person.",
  },
  {
    q: "Does my data leave my Mac?",
    a: "With the free app, your synced data stays on your Mac. Your questions and the relevant snippets go only to the AI you chose, or nowhere at all if you use a local model through Ollama. With Heed Cloud, yes, and we'd rather be upfront about it: the apps you connect sync to your account in our data centre (planned for London), and the relevant snippets go to our AI provider (Anthropic, or OpenAI as a fallback) to produce each answer.",
  },
  {
    q: "Do you sell my data or use it to train AI?",
    a: "Never. We don't sell or share your data for advertising, and we don't train models on it. Our AI providers' API terms say they don't train on API data by default; they keep requests for a limited time for abuse monitoring.",
  },
  {
    q: "Which apps can I connect?",
    a: `${appList()}. More coming soon. Connecting takes one click: you approve access with the app itself, and Heed never sees your password.`,
  },
  {
    q: "Can I use my ChatGPT or Claude subscription?",
    a: "With the free app, yes for ChatGPT: use OpenAI's Sign in with ChatGPT and your Plus or Pro plan covers it. For Claude, use an Anthropic API key, because Anthropic doesn't allow Pro or Max logins in third-party apps. You can also use an OpenAI or OpenRouter key, or a local model through Ollama. Heed Cloud includes its AI, so you don't need a key at all.",
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
    q: "Can I self-host the cloud?",
    a: "Yes. Heed is open core, and the cloud service is in the same repository. There's a community-supported self-hosting guide. It comes with no warranty or support from us.",
    link: { href: "/docs/self-host", text: "Read the self-hosting guide" },
  },
  {
    q: "How do I cancel or delete my account?",
    a: "Cancel any time from Manage billing on your account page; you keep access until the end of the period you paid for. Deleting your account disconnects every app and deletes all your synced data from our database.",
  },
];
