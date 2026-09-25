<!--
  DRAFT — generated to reflect Nels's actual data flows (AI processing,
  Stripe billing, Stripe Financial Connections bank linking, Fly.io hosting,
  email-based sharing, TOTP auth).
  Have a lawyer review before relying on this for a public launch. A personal-finance
  app handling financial data should not ship boilerplate unreviewed.
-->
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import { SITE } from '$lib/site.js';

  const updated = 'September 25, 2026';
</script>

<Seo
  title="Privacy Policy"
  path="/privacy"
  localized={false}
  description="How Nels collects, uses, and protects your data — including how your budget data is processed by AI, who our subprocessors are, and your rights over your information."
/>

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">Privacy Policy</h1>
  <p class="mt-2 text-sm opacity-60">Last updated: {updated}</p>

  <div class="prose mt-6 max-w-none opacity-90">
    <p>{SITE.name} is operated by Savvagent, LLC. This Privacy Policy explains what information we collect when you use our budgeting app and website, how we use it, who we share it with, and the choices you have. By using {SITE.name}, you agree to this policy.</p>

    <h2>Information we collect</h2>
    <ul>
      <li><strong>Account information.</strong> Your email address and the credentials for your authenticator app (we store a secret used to verify your time-based one-time passwords, encrypted at rest; we never store a reusable password).</li>
      <li><strong>Budget data you provide.</strong> Budgets, categories, limits, expenses, income, savings goals, collaborators, and the messages you send to the {SITE.name} assistant.</li>
      <li><strong>Payment information.</strong> When you subscribe, payment is processed by our payment provider (Stripe). We receive billing status and the last four digits of your card; we do not store full card numbers.</li>
      <li><strong>Bank account data (optional, Pro).</strong> If you choose to link a bank account, we receive information about that account through Stripe Financial Connections — the institution name, account name and type, a masked account number, and transaction history (dates, amounts, and descriptions). We never see or store your bank login credentials; you authenticate directly with your bank through Stripe.</li>
      <li><strong>Technical data.</strong> Standard server logs (such as IP address, browser type, and timestamps) needed to operate and secure the service.</li>
    </ul>

    <h2>How we use your information</h2>
    <ul>
      <li>To provide the core service — storing your budgets, parsing expenses, and answering your questions.</li>
      <li>To generate AI insights, forecasts, and categorizations from the data you provide.</li>
      <li>To automatically import transactions from bank accounts you link, keep them up to date, and include them in your budgets and insights.</li>
      <li>To authenticate you, enable sharing, and maintain the collaboration audit log.</li>
      <li>To process subscriptions and prevent fraud and abuse.</li>
      <li>To communicate with you about your account and respond to support requests.</li>
    </ul>

    <h2>AI processing</h2>
    <p>{SITE.name} uses third-party large-language-model providers to interpret your messages and generate responses, categorizations, and insights. We send only what is needed for each of these tasks. {SITE.name} does not sell your data and does not build advertising profiles. How an AI provider itself handles the data is governed by that provider's terms, described below.</p>
    <p>{SITE.name} sends the following to the AI provider:</p>
    <ul>
      <li>your chat messages, with the budget details needed to answer them;</li>
      <li>transaction descriptions, to build search embeddings, including transactions added outside chat (for example imported from a linked bank or entered on a form);</li>
      <li>conversation text, to generate conversation titles and suggested questions;</li>
      <li>budget and spending figures, to write spending-report narratives.</li>
    </ul>
    <p>
      All of these follow the same provider choice, with one exception: search embeddings are
      only ever created with Google's Gemini API. If you use your own OpenAI or Anthropic key,
      embeddings are off, and smart transaction search and conversation memory are unavailable.
    </p>
    <p>
      By default, {SITE.name} sends this data to Google's Gemini API under {SITE.name}'s paid account.
      Google's Gemini API terms for paid services govern how Google handles that data;
      see <a href="https://ai.google.dev/gemini-api/terms">Google's Gemini API terms</a>.
      Those terms state that for paid services Google "doesn't use your prompts (including
      associated system instructions, cached content, and files such as images, videos, or
      documents) or responses to improve our products", and that "Google logs prompts and
      responses for a limited period of time, solely for detecting and preventing violations
      of the Prohibited Use Policy to maintain the safety and security of the Services, and
      any required legal or regulatory disclosures."
    </p>
    <p>
      You can instead use your own API key for Google Gemini, OpenAI or Anthropic in
      Settings → AI provider. Your requests then go to that provider under your own
      account and your own agreement with them (see
      <a href="https://developers.openai.com/api/docs/guides/your-data">OpenAI's API data controls</a> and
      <a href="https://www.anthropic.com/legal/commercial-terms">Anthropic's Commercial Terms of Service</a>),
      and {SITE.name} stores your key encrypted.
      In every case {SITE.name}'s servers build each request from your data, so {SITE.name} still
      processes it; your own key changes who receives it, not whether {SITE.name} sees it.
    </p>
    <p>
      <strong>Free-tier Gemini keys.</strong> If your own Gemini key is from Google's free (unpaid)
      tier, such as a free Google AI Studio key, Google's terms for unpaid services apply to those
      requests instead. Under those terms Google may use your prompts and its responses to improve
      its products: "Google uses the content you submit to the Services and any generated responses
      to provide, improve, and develop Google products and services and machine learning
      technologies". Use a key from a paid (billing-enabled) Google account if you don't want that.
    </p>
    <p><strong>Shared budgets.</strong> Each person's own provider choice applies to their own chats, in both directions:</p>
    <ul>
      <li>If you use your own key and share a budget, a collaborator who uses the default sends that budget's details to Google's Gemini API through {SITE.name}.</li>
      <li>If someone shares a budget with you and you use your own key, your chats about it send the owner's budget details to your provider account, under your agreement with that provider.</li>
    </ul>

    <h2>Who we share data with</h2>
    <p>We do not sell your personal information. We share data only with service providers ("subprocessors") that help us run {SITE.name}, under contracts that limit their use of it. The one exception is an AI provider you choose with your own API key: your own agreement with that provider governs those requests, not a contract with {SITE.name}.</p>
    <ul>
      <li><strong>AI providers: Google (Gemini API) by default; OpenAI or Anthropic only if you choose them with your own key</strong> — AI processing of your chat messages, budget details, and transaction descriptions, as described under "AI processing" above.</li>
      <li><strong>Stripe</strong> — subscription billing, payment processing, and bank account connections (Stripe Financial Connections). When you link a bank account, you interact with Stripe directly and Stripe collects information from you and your financial institution under its own <a href="https://stripe.com/privacy">privacy policy</a>. If we add bank connections for other regions, we will use comparable regulated providers and update this policy.</li>
      <li><strong>Fly.io</strong> — application hosting.</li>
      <li><strong>Managed PostgreSQL hosting</strong> — storage of your account and budget data, encrypted at rest.</li>
    </ul>
    <p>We may also disclose information if required by law, or to protect the rights, safety, and security of our users and the service.</p>

    <h2>Sharing and collaboration</h2>
    <p>When you invite someone to a budget by email, the data in that budget becomes visible to them according to the permission you grant (view, edit, or owner). You are responsible for who you invite. Every change to a shared budget is recorded in an audit log visible to collaborators.</p>

    <h2>Data retention and deletion</h2>
    <p>We keep your data for as long as your account is active. You can delete individual budgets at any time, and you can delete your account, which removes your personal data and budget content from active systems. Residual copies may remain in encrypted backups for a limited period before being overwritten, and we may retain limited records where required for legal, tax, or fraud-prevention purposes.</p>
    <p>You can unlink a bank account at any time from the Accounts page. Unlinking revokes our access to that account through Stripe and stops all future imports. Transactions already imported remain part of your budget — like any other budget data, you can delete them individually, with their budget, or by deleting your account.</p>

    <h2>Security</h2>
    <p>Sign-in is passwordless and based on TOTP authenticator apps, so there is no reusable password to leak. Your authenticator secret — the only credential that can sign you in — is encrypted at rest at the application layer (AES-256-GCM), so it is never stored in a readable form. Your account and budget data are encrypted at rest by our managed database host and encrypted in transit (HTTPS everywhere). No system is perfectly secure, but we take reasonable measures to protect your information and design the product to minimize what we collect.</p>

    <h2>Your rights</h2>
    <p>Depending on where you live, you may have the right to access, correct, export, or delete your personal data, and to object to or restrict certain processing. You can exercise most of these directly in the app, or by contacting us at <a href={`mailto:${SITE.supportEmail}`}>{SITE.supportEmail}</a>. We will not discriminate against you for exercising these rights.</p>

    <h2>Children</h2>
    <p>{SITE.name} is not directed to children and is not intended for anyone under 16. We do not knowingly collect data from children. If you believe a child has provided us data, contact us and we will delete it.</p>

    <h2>International transfers</h2>
    <p>Our service providers may process data in countries other than your own. Where required, we rely on appropriate safeguards for those transfers.</p>

    <h2>Changes to this policy</h2>
    <p>We may update this policy from time to time. When we make material changes, we will update the date above and, where appropriate, notify you.</p>

    <h2>Contact</h2>
    <p>Questions about this policy or your data? Email us at <a href={`mailto:${SITE.supportEmail}`}>{SITE.supportEmail}</a>, or write to us at:</p>
    <p>Savvagent, LLC<br />707 W Halvorsen Parkway<br />Saratoga Springs, Utah 84045</p>

    <p class="text-sm opacity-70"><em>{SITE.name} provides general budgeting tools, not financial advice.</em></p>
  </div>
</Section>
