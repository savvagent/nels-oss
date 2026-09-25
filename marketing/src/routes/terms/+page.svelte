<!--
  DRAFT — reflects Nels's actual model (trial-then-paywall subscription, AI assistant,
  email-based sharing, TOTP auth). Have a lawyer review before a public launch.
-->
<script>
  import Seo from '$lib/components/Seo.svelte';
  import Section from '$lib/components/Section.svelte';
  import { SITE } from '$lib/site.js';

  const updated = 'June 11, 2026';
</script>

<Seo
  title="Terms of Service"
  path="/terms"
  localized={false}
  description="The terms that govern your use of Nels — including subscriptions, billing, acceptable use, sharing, and the limits of our AI-generated insights."
/>

<Section classes="max-w-2xl">
  <h1 class="text-4xl font-bold">Terms of Service</h1>
  <p class="mt-2 text-sm opacity-60">Last updated: {updated}</p>

  <div class="prose mt-6 max-w-none opacity-90">
    <p>These Terms of Service ("Terms") govern your use of {SITE.name} (the "Service"), operated by Savvagent, LLC ("we," "us"). By creating an account or using the Service, you agree to these Terms. If you do not agree, do not use the Service.</p>

    <h2>The Service</h2>
    <p>{SITE.name} is a budgeting co-pilot that lets you manage budgets, track expenses, share with others, and receive AI-generated insights and forecasts. We may add, change, or remove features over time.</p>

    <h2>Your account</h2>
    <p>You must provide accurate information and be at least 16 years old to use {SITE.name}. Sign-in uses a TOTP authenticator app; you are responsible for keeping your authenticator device and recovery method secure, and for all activity under your account. Notify us promptly at <a href={`mailto:${SITE.supportEmail}`}>{SITE.supportEmail}</a> if you suspect unauthorized access.</p>

    <h2>Subscriptions, trial, and billing</h2>
    <ul>
      <li><strong>Free trial.</strong> {SITE.name} offers a {SITE.trialDays}-day free trial. After the trial ends, continued use requires a paid subscription.</li>
      <li><strong>Pricing.</strong> Plans are billed monthly or annually — Basic at {SITE.plans.basic.monthly}/month or {SITE.plans.basic.annual}/year, and Pro at {SITE.plans.pro.monthly}/month or {SITE.plans.pro.annual}/year. Prices may change with notice; changes do not affect the current paid period.</li>
      <li><strong>Auto-renewal.</strong> Subscriptions renew automatically at the end of each billing period until you cancel.</li>
      <li><strong>Cancellation.</strong> You can cancel at any time. Cancellation stops future renewals; you keep access through the end of the period you have already paid for.</li>
      <li><strong>Refunds.</strong> Except where required by law, payments are non-refundable. Because we offer a {SITE.trialDays}-day free trial before any charge, you can evaluate the Service without payment.</li>
    </ul>
    <p>Payments are processed by Stripe, and are also subject to Stripe's terms. If you link a bank account (a Pro feature), the connection is provided through Stripe Financial Connections and is likewise subject to Stripe's terms; transactions imported this way are treated as data in your account under these Terms and our Privacy Policy.</p>

    <h2>Acceptable use</h2>
    <p>You agree not to misuse the Service. In particular, you will not: break the law; infringe others' rights; attempt to access accounts or data that are not yours; probe, scan, or disrupt the Service; reverse-engineer or resell it; or upload malicious content. We may suspend or terminate accounts that violate these Terms.</p>

    <h2>Sharing and collaboration</h2>
    <p>You may invite others to your budgets and grant them view, edit, or owner permissions. You are responsible for who you invite and what you share. Collaborators can see and, depending on their permission, change the shared budget; all changes are recorded in an audit log.</p>

    <h2>Not financial advice</h2>
    <p>{SITE.name} provides general budgeting tools and AI-generated insights for informational purposes only. It is not financial, investment, tax, or legal advice. AI output can be incomplete or wrong — you are responsible for verifying it and for your own financial decisions.</p>

    <h2>Your content</h2>
    <p>You own the data you put into {SITE.name}. You grant us a limited license to store and process that data solely to operate and provide the Service to you, including sending it to our third-party AI providers to generate responses, as described in our <a href="/privacy">Privacy Policy</a>.</p>

    <h2>Intellectual property</h2>
    <p>The Service, including its software, design, and branding, belongs to us and our licensors. These Terms do not grant you any rights in them except the right to use the Service as permitted.</p>

    <h2>Disclaimers</h2>
    <p>The Service is provided "as is" and "as available," without warranties of any kind, whether express or implied, to the fullest extent permitted by law. We do not warrant that the Service will be uninterrupted, error-free, or that AI insights will be accurate.</p>

    <h2>Limitation of liability</h2>
    <p>To the fullest extent permitted by law, {SITE.name} and its operators will not be liable for any indirect, incidental, special, consequential, or punitive damages, or for any loss of data, profits, or savings, arising from your use of the Service. Our total liability for any claim is limited to the amount you paid us in the 12 months before the claim.</p>

    <h2>Termination</h2>
    <p>You may stop using the Service and delete your account at any time. We may suspend or terminate your access if you violate these Terms or if necessary to protect the Service or other users. Sections that by their nature should survive termination will do so.</p>

    <h2>Governing law</h2>
    <p>These Terms are governed by the laws of the State of Utah, United States, without regard to conflict-of-laws rules, and you agree to the exclusive jurisdiction of the state and federal courts located in Utah.</p>

    <h2>Changes to these Terms</h2>
    <p>We may update these Terms from time to time. When we make material changes, we will update the date above and, where appropriate, notify you. Continued use after changes take effect means you accept the updated Terms.</p>

    <h2>Contact</h2>
    <p>Questions about these Terms? Email us at <a href={`mailto:${SITE.supportEmail}`}>{SITE.supportEmail}</a>, or write to us at:</p>
    <p>Savvagent, LLC<br />707 W Halvorsen Parkway<br />Saratoga Springs, Utah 84045</p>
  </div>
</Section>
