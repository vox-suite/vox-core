# Platform V1 provider feasibility and selection record

**Decision date:** 2026-09-19  
**Source access date:** 2026-09-19  
**Scope:** `vox-core#2` / E02 — Amazon, Expedia, Zomato, and Uber  
**Evidence standard:** Public, first-party provider documentation and terms only

> **2026-09-23 provider check:** Uber's current [GET /v3/me documentation](https://developer.uber.com/docs/consumer-identity/references/api/v3/me-get) explicitly says this endpoint requires Uber approval, even though `profile` is labelled a general scope in the [Riders scopes guide](https://developer.uber.com/docs/riders/guides/scopes). A general OAuth scope is not proof that Vox may retrieve a stable rider identity. The selected connected-read route remains disabled until Uber confirms access to the identity and history endpoints for the Vox application and live account tests pass. Recheck the current scopes and endpoint versions before implementing the production adapter.

## Executive decision

Platform V1 should use the following routes:

1. **Connected read: Uber rider trip history**, using user OAuth with the general `history` or data-minimized `history_lite` scope. Uber documents those scopes as general rather than privileged and documents `GET /history` as a user-token endpoint. This is the lowest-authority named integration that demonstrates a real user connection without granting execution authority ([Uber scopes](https://developer.uber.com/docs/riders/guides/scopes), [Uber authentication](https://developer.uber.com/docs/riders/guides/authentication/introduction)).
2. **Consequential write: Expedia Rapid Lodging**, limited initially to lodging booking, retrieval, cancellation, and reconciliation. Rapid documents the entire lifecycle and a production onboarding path, but production must remain **disabled** until Expedia approves Vox as a partner, supplies credentials and commercial/payment configuration, and passes the implementation through site review ([Rapid getting started](https://developers.expediagroup.com/docs/products/rapid/setup/getting-started), [Rapid Lodging overview](https://developers.expediagroup.com/rapid/lodging), [Manage Booking](https://developers.expediagroup.com/rapid/lodging/manage-booking/about-mg-booking-api)).
3. **Connected-write release gate:** If Expedia approval and production validation are not complete at the Platform V1 release freeze, select another officially authorized provider that can execute an exact approved consequential action and return an authoritative outcome. Keep the Expedia adapter sandbox-only or as a labelled handoff. A Vox-owned reminder is a separate P0 feature and cannot satisfy the PRD's connected consequential-write requirement.
4. **Amazon, Zomato, and Uber ride ordering:** ship only the verified read subset and an explicitly labelled handoff. Opening Amazon, Zomato, Expedia, or Uber is not completion. Post-handoff outcomes remain unknown unless an authoritative provider mechanism later proves them.

These choices are global-platform choices, not promises of universal service coverage. Capability discovery must still evaluate account, product, point of sale, location, currency, inventory, and provider approval at runtime.

## Evidence labels and capability levels

- **Verified:** explicitly stated by a current public first-party source.
- **Inference:** a conservative conclusion drawn from the verified public surface. It is not a provider promise.
- **Unknown:** not established by the reviewed public sources.
- **Handoff:** Vox may transfer the user to the provider, but must not claim the external action or outcome.

Capability levels used below:

- **L0 — labelled handoff only**
- **L1 — public or partner catalog/discovery read**
- **L2 — connected user read**
- **L3 — consequential write with authoritative reconciliation**

## Dated capability matrix

| Provider | Verified public/partner surface | Authentication and access | Region evidence | Payment responsibility | Cancellation and authoritative outcome | Decision for Platform V1 |
|---|---|---|---|---|---|---|
| Amazon | Creators API catalog search, item, variation, browse-node, price/offer metadata, and Amazon detail-page links | OAuth 2.0 client credentials; fully accepted Associates account, qualified-sales eligibility, marketplace Partner Tag | 22 documented marketplace locales; approval/tag requirements remain marketplace-specific | Checkout and payment occur at Amazon; no consumer purchase API is documented | No buyer-order, cancel, refund, or per-user purchase status operation in the published Creators API operation list | **L1 if approved; otherwise L0.** Catalog read plus Amazon handoff only |
| Expedia | Rapid shopping, booking, retrieve, change, cancel, notification, and receipt surfaces | Partner application; API key + shared secret signed requests; restricted development until site review and production approval | Geography covers 600,000+ regions/airports; bookable supply and point-of-sale support are dynamic and contractual | Expedia Collect, Property Collect, or approved affiliate/partner model; PCI and possibly 3DS/SCA obligations vary | Retrieve/Manage Booking and notifications provide reconciliation; cancellation penalties/refunds are itinerary/rate-specific | **Conditional L3 for lodging.** Selected write provider only after all gates pass |
| Zomato | Current developer portal is a restaurant/POS integration for menu, outlet, and inbound-order operations; a separate content API policy describes licensed restaurant information | POS access requires Zomato POC onboarding, API keys, NDA, whitelisting, and eligibility; public content policy refers to a provider-issued key | Current POS documentation is India-localized; no reviewed source establishes a global consumer API | Zomato platform and restaurant handle customer payment/settlement; Vox has no documented consumer payment role | POS APIs expose restaurant-side order states/cancellation requests, not a consumer create-order or connected consumer outcome API | **L0 for consumer use.** Labelled Zomato handoff only |
| Uber | Rider profile/history/places reads; estimates; privileged ride request; ride status, cancellation and receipts for qualifying applications | OAuth 2.0 user token. `history`/`history_lite` are general; `request`, `request_receipt`, and `all_trips` are privileged and need Full Access for the public | Uber lists service in 15,000+ cities, but products and request eligibility are coordinate/account-specific | The rider's Uber account and on-file payment method are charged; Vox must never handle raw payment credentials | Request status can be polled; cancel is supported; receipt is limited to rides originated by the app and may include cancellation charge | **L2 selected read. L0 for rides until privileged production approval and live validation** |

## Amazon

### Verified facts

- Amazon states that Product Advertising API 5.0 is deprecated and that the **Creators API** is its supported catalog successor for publishers, influencers, and affiliate partners ([PA-API deprecation notice](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/paapiv5-deprecation)).
- The published Creators API reference lists catalog operations—Get Browse Nodes, Get Items, Search Items, and Get Variations—and product/offer resources. It does not publish a buyer checkout, purchase, order-history, cancellation, or refund operation ([Creators API reference](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/api-reference)). Product data includes availability and price/offer metadata, and item data can direct a customer to the Amazon detail page ([OffersV2](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/api-reference/resources/offersV2), [Item Info](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/api-reference/resources/item-info)).
- Authentication is OAuth 2.0 `client_credentials`; bearer tokens last one hour. Amazon documents marketplace endpoints and a requirement for a valid Partner Tag and approved access for the target marketplace ([Creators API cURL guide](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/get-started/using-curl)).
- The documented locale set is Australia, Belgium, Brazil, Canada, Egypt, France, Germany, India, Ireland, Italy, Japan, Mexico, Netherlands, Poland, Singapore, Saudi Arabia, Spain, Sweden, Turkey, United Arab Emirates, United Kingdom, and United States ([locale reference](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/locale-reference)). This is a marketplace list, not a guarantee that any product is purchasable or deliverable everywhere in a marketplace.
- Registration is limited to finally accepted Amazon Associates who have referred qualified sales. Amazon's error documentation states a current eligibility threshold of 10 qualified sales in the trailing 30 days; rates depend on shipped revenue and access can be lost after 30 consecutive days without qualified referring sales ([registration](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/onboarding/register-for-creators-api), [errors](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/troubleshooting/error-codes-and-messages), [API rates](https://affiliate-program.amazon.com/creatorsapi/docs/en-us/concepts/api-rates)).
- Amazon Selling Partner API is an API for sellers and vendors managing selling operations, not a consumer purchasing API ([SP-API onboarding overview](https://developer-docs.amazon.com/sp-api/docs/onboarding-overview)).

### Inference and classification

Amazon is **catalog read plus handoff**, not a direct commerce executor. A compliant adapter may display a currently returned catalog offer and Amazon link, with required attribution and locale terms, then hand the user to Amazon. Amazon and the customer own checkout, payment authentication, cancellation, refunds, and the final order record. Affiliate conversion reporting must not be treated as per-user authoritative order reconciliation unless Amazon separately confirms that use.

Direct execution is **not realistically available to a new general-purpose third-party platform globally** through the reviewed consumer-facing developer surface. Even the read path is gated by marketplace enrollment and sales performance.

### Unknowns and provider follow-up

- Obtain Associates approval and a Partner Tag for every intended launch marketplace.
- Obtain written confirmation that an assistant UI, generated recommendations, caching policy, price display, attribution, and deep-link behavior comply with each locale's Associates/IP terms.
- Ask Amazon whether any approved reporting surface can lawfully and reliably reconcile a purchase to a particular user-authorized Vox action. Until confirmed, outcome is `unknown` after handoff.
- Validate live rate limits and revocation behavior with production credentials; do not build the release read proof on assumed access.

## Expedia

### Verified facts

- Rapid requires a company to apply and be approved as a partner. Approved partners receive an API key and shared secret; requests use an SHA-512 signature. Credentials remain in restricted development mode until Expedia's site review and production approval ([Rapid getting started](https://developers.expediagroup.com/docs/products/rapid/setup/getting-started)).
- Rapid Lodging supports shopping and actual booking. A successful create response returns an itinerary ID and links to retrieve, cancel, or resume; Retrieve returns dates, rates, room data, confirmation numbers, and current booking status ([Rapid Lodging overview](https://developers.expediagroup.com/rapid/lodging)).
- Manage Booking explicitly supports Retrieve, Change, and Cancel. Prebuilt retrieve and room-cancel links are returned from successful create responses ([Manage Booking](https://developers.expediagroup.com/rapid/lodging/manage-booking/about-mg-booking-api)).
- A unique `affiliate_reference_id` is required to prevent duplicate submissions and locate uncertain bookings. Expedia says a timeout or 5xx is not proof of failure; the client must retrieve using the same reference and email, retry reconciliation, and escalate unresolved cases rather than making a new booking with a new reference ([launch requirements](https://developers.expediagroup.com/rapid/setup/launch-requirements/lodging-launch-reqs), [common errors and reconciliation](https://developers.expediagroup.com/rapid/lodging/reference/error-responses)).
- Notifications cover booking creation, changes, supplier/agent/fraud cancellations, refunds, and other itinerary changes; the receiver should call Retrieve after a notification to obtain authoritative current state ([Notifications](https://developers.expediagroup.com/rapid/lodging/notifications/notifications)).
- Cancellation policy and refundability are rate- and time-dependent. Shopping exposes penalties, while the booked itinerary and cancel-refund response are the post-booking source of truth ([cancellation policies](https://developers.expediagroup.com/rapid/lodging/shopping/constructing-cancellation-policies)).
- Payment responsibility varies. Property Collect requires the traveler to supply card details and ordinarily pay the property; handling those card details requires PCI compliance. In some SCA cases Rapid becomes merchant of record. Expedia Collect has Expedia collect payment from the traveler; receipt responsibilities differ by model ([Property Collect](https://developers.expediagroup.com/rapid/lodging/booking/property-collect), [SCA/PSD2](https://developers.expediagroup.com/rapid/lodging/booking/sca-regulation), [booking receipts](https://developers.expediagroup.com/rapid/lodging/manage-booking/booking-receipts)). Platform V1's rule against raw payment credentials means Vox needs an Expedia-approved hosted/tokenized payment and 3DS design; public docs alone do not establish that configuration for Vox.
- Geography APIs describe more than 600,000 regions and airports and support worldwide hierarchy requests. This is strong evidence of global discovery breadth, but not universal bookability or a promise for every point of sale ([Geography API](https://developers.expediagroup.com/rapid/lodging/geography/about-geography-api)).

### Inference and classification

Expedia Rapid Lodging is the strongest **conditional L3** candidate because it exposes both the external effect and the mechanisms needed to distinguish booked, cancelled, failed, pending, and unresolved outcomes. It is plausibly available to a new travel platform through a documented commercial application, but it is not self-serve production access and not globally uniform.

Selection does **not** authorize implementation to advertise live bookings. The adapter must declare direct booking unavailable until the operator installs a provider-approved profile that has passed site review, payment/security review, and live reconciliation tests. Unsupported points of sale, inventory, currencies, property changes, refund paths, and other Expedia product lines become labelled handoff or human support—not optimistic execution.

### Unknowns and provider/human follow-up

- **Commercial owner:** apply for Rapid partnership and obtain written approval for lodging, intended consumer/agent model, launch countries and points of sale, currencies, inventory, rate limits, fees, support obligations, and data-processing terms.
- **Payments/security owner:** agree the merchant-of-record model, tokenization/hosted collection path, PCI scope, 3DS/SCA flow, refund settlement, billing descriptor, and prohibition on raw card data entering Vox or agent context.
- **Engineering owner:** obtain test credentials, implement with one `affiliate_reference_id` per approved proposal, prove timeout/duplicate reconciliation, verify notification signatures/delivery behavior, and retain only minimal audit evidence.
- **Product/operations owner:** pass Expedia's site review and document 24/7 escalation for unresolved bookings, supplier cancellations, refunds, and stranded travelers.
- Validate the exact production region/currency matrix from the approved partner profile. Public worldwide geography is not sufficient access proof.

## Zomato

### Verified facts

- Zomato's current developer portal describes a **POS integration for restaurant partners**: menu management, outlet management, and order handling after a customer places an order on Zomato ([POS overview](https://www.zomato.com/developer/integration/docs/overview/), [Order Management](https://www.zomato.com/developer/integration/docs/api-documentation/order-management/)). It exposes confirm, reject, ready, picked-up, delivered, customer cancellation relay, and order-status webhooks from the restaurant/POS side.
- POS onboarding requires a Zomato point of contact, vendor forms, a Zomato-created POS ID, API keys, an NDA, and domain/email/IP whitelisting ([pre-integration guide](https://www.zomato.com/developer/integration/docs/getting-started/development-for-integration/pre-integration/)). Eligibility requires at least 50 onboarded restaurants or 10,000 monthly orders, 100% critical-feature parity, 24/7 operations with a sub-10-minute response commitment, and greater than 99.999% uptime; Zomato retains the final eligibility decision ([prerequisites](https://www.zomato.com/developer/integration/docs/getting-started/prerequisites/)).
- A separate API policy describes licensed restaurant information, a secure API key, a 1,000-call/day limit, mandatory attribution, restrictions on storage/caching/commingling, and Zomato's right to suspend the key ([API policy](https://www.zomato.com/policies/api-policy/)). The reviewed public sources do not establish a current self-service signup or production credential route for this content surface.
- Zomato's consumer terms say the customer pays the order through the Zomato platform and describe provider/restaurant-controlled cancellation, liquidated damages, and refund decisions; eligible refunds return through the relevant provider mechanism ([consumer Terms of Service](https://www.zomato.com/policies/terms-of-service/)). Restaurant terms describe Zomato/payment-mechanism collection and later settlement to the restaurant ([online ordering terms](https://www.zomato.com/policies/online-ordering/)).
- The current POS portal is India-localized and its prerequisites are framed for Zomato restaurant partners. No reviewed official source establishes a global consumer account API, consumer order-creation API, or consumer order-history/reconciliation API.

### Inference and classification

The POS surface cannot be repurposed as a customer assistant ordering API: it starts after a Zomato customer order reaches a restaurant, uses merchant credentials and operational duties, and does not authorize Vox to create an order for a consumer. The older content license may support a narrow restaurant-information read for an approved key, but access and current endpoints are unverified.

Zomato is therefore **L0 labelled handoff** for Platform V1 consumer journeys. Direct consumer ordering is not realistically available to a new global third-party platform based on the reviewed public production surface. POS order events are authoritative for restaurant operations, not for a Vox user's authorization or payment outcome.

### Unknowns and provider follow-up

- Ask `pos-partnership@zomato.com` or an accountable Zomato business owner whether a consumer discovery/order partner API exists and obtain written permitted-use, country, currency, auth, payment, status, cancellation, refund, support, and data-retention terms.
- Ask `api@zomato.com` whether the restaurant-content API still accepts new applications and request current endpoint, region, attribution, storage, and rate-limit documentation.
- Do not use POS credentials, restaurant webhooks, reverse-engineered mobile endpoints, or browser automation for a consumer integration.
- Until a consumer API is contractually approved and live-tested, transfer only to Zomato and keep the Vox outcome as handoff/unknown.

## Uber

### Verified facts

- Uber's Riders API uses OAuth 2.0 user access tokens for profile, history, places, request estimates, ride requests, ride details, cancellations, maps, and qualifying receipts. Uber no longer grants new server tokens ([authentication](https://developer.uber.com/docs/riders/guides/authentication/introduction)).
- `profile`, `history`, `history_lite`, `offline_access`, and `places` are general scopes. `request`, `request_receipt`, and `all_trips` are privileged. Privileged scopes work only for the developer team by default; public production use requires Full Access review and a public privacy policy ([scopes](https://developer.uber.com/docs/riders/guides/scopes)).
- The ride-request flow requires a short-lived upfront `fare_id`, pickup/destination, an OAuth token with `request`, and an Uber-approved production scope. The API charges the rider's Uber account, and the rider must already have a valid payment method on file ([ride request tutorial](https://developer.uber.com/docs/riders/ride-requests/tutorials/api/curl), [Create Request](https://developer.uber.com/docs/riders/references/api/v1.2/requests-post)).
- Product data can expose currency, cash availability, service fees, and the cancellation fee after the grace period ([Products endpoint](https://developer.uber.com/docs/riders/references/api/v1.2/products-get)). Cancellation is available through `DELETE /requests/{request_id}`; status values include processing, no drivers, accepted, arriving, in progress, driver-cancelled, rider-cancelled, and completed. Uber says clients should treat `GET /requests/{request_id}` as the authoritative current state because the rider can also act in the Uber app ([Ride Request best practices](https://developer.uber.com/docs/riders/ride-requests/tutorials/api/best-practices)).
- `request_receipt` only returns receipts for requests created by the application. A late cancellation charge can appear on that receipt. Uber's webhook documentation warns that webhook events may be duplicated or out of order and identifies `event_id` for deduplication; the historical Riders webhook page also labels some status event types deprecated, so production callback support needs explicit reconfirmation ([webhooks](https://developer.uber.com/docs/riders/guides/webhooks), [scopes restrictions](https://developer.uber.com/docs/riders/guides/scopes)).
- Uber advertises rides in more than 15,000 cities across a long country list, but API request errors still include product/account/jurisdiction restrictions and `outside_service_area`. Service must be discovered by actual coordinates and account eligibility ([Uber cities](https://www.uber.com/gb/en/r/cities/), [Create Request errors](https://developer.uber.com/docs/riders/references/api/v1.2/requests-post)).
- Uber provides a sandbox in which `POST /requests`, state transitions, and cancellation can be exercised without dispatching or charging a real ride. Sandbox behavior does not prove production location/account eligibility ([sandbox](https://developer.uber.com/docs/riders/guides/sandbox)).

### Inference and classification

Uber `history_lite` is the best named **L2 connected-read** proof: it uses user consent, can minimize city/location data, and does not grant ride-request authority. The adapter should request `history` only when the city field is materially required.

Uber ride request is technically capable of L3, but it is **not selected for Platform V1 execution**. Full Access is discretionary, receipts cover only app-originated trips, some webhook documentation is deprecated, and actual service is account/location/product dependent. Until Uber grants production scopes and Vox validates request, duplicate handling, polling, cancellation fee, receipt, and unknown-outcome recovery, rides remain labelled handoff. A new third-party can build and submit for review, but public docs do not establish globally available production approval.

### Unknowns and provider follow-up

- Register a developer application and live-test the general `history_lite` OAuth grant with a consenting test user; confirm whether Uber imposes any additional production review on the intended user volume.
- Ask Uber for current production guidance on Riders API longevity, supported webhook event types, polling limits, rate limits, data retention/deletion, and Full Access review criteria.
- If ride write is reconsidered, obtain `request` and `request_receipt` Full Access in writing; validate live requests in expressly permitted locations/accounts and prove cancellation fee and receipt behavior.
- Never ingest raw payment credentials. If the Uber account lacks a valid method or needs identity/payment remediation, use provider authentication/handoff and do not claim a requested ride.

## Required enablement gates

No production adapter may declare a capability above these researched levels. In addition, the following evidence is required before enabling each selected route:

### Uber connected read

- Developer application registration and real OAuth credentials.
- Provider confirmation that the Vox application can use a stable rider-identity endpoint; the current `/v3/me` documentation states that approval is required.
- Consent screen and privacy policy reviewed against the exact `history_lite`/`history` fields.
- Successful connect, refresh, revoke, disconnect, and cross-user-isolation tests.
- Provider response captured in redacted form, including region/account behavior and rate-limit headers where available.
- Retention/deletion behavior approved by the privacy owner.

If any item is missing, the capability is disabled; Uber handoff may remain.

### Expedia consequential write

- Signed/accepted partner and data terms plus documented operator/commercial owner.
- Production profile with explicit countries, points of sale, currencies, lodging inventory, merchant-of-record/payment model, rate limits, and support path.
- An approved payment architecture that keeps raw card/bank data out of Vox and agent context, including PCI and 3DS/SCA sign-off where applicable.
- Expedia site review passed.
- Permitted end-to-end evidence for quote expiry/price change, approval, booking success, payment rejection, provider authentication, timeout/unknown, duplicate delivery, cancellation, refund, supplier cancellation, and notification/retrieve reconciliation.
- Operational runbook for unresolved/stranded bookings and a kill switch that disables booking before read/status.

If any item is missing at release freeze, keep Expedia's production write disabled and expose only shopping or handoff at the level actually approved. Select and validate another officially authorized connected consequential-write provider, or leave the release blocked. Reminder scheduling and delivery still need their own P0 evidence, but do not substitute for this gate.

## Implementation handoff

This research ticket fixes the maximum truthful capability level; it does not
enable either provider. The implementation tickets must preserve these gates:

| Selected route | Implementation issue | Contract handed downstream |
|---|---|---|
| Uber `history_lite` connected read | [`vox-core#19`](https://github.com/vox-suite/vox-core/issues/19) | A user-authorized, data-minimized read whose availability is discovered per account and whose expired/revoked access becomes `reconnect_required` rather than fabricated data |
| Expedia Rapid Lodging consequential write | [`vox-core#20`](https://github.com/vox-suite/vox-core/issues/20) | An exact approved booking intent, provider authentication distinct from platform approval, stable idempotency identity, and authoritative retrieval/cancellation/reconciliation including `unknown` outcomes |

Issues 19 and 20 may implement a provider adapter before production access is
granted, but they must keep its production capability disabled until every
enablement gate above has evidence. The named-provider issues for
[Amazon](https://github.com/vox-suite/vox-core/issues/21),
[Expedia](https://github.com/vox-suite/vox-core/issues/22),
[Zomato](https://github.com/vox-suite/vox-core/issues/23), and
[Uber](https://github.com/vox-suite/vox-core/issues/24) may only expose the
levels recorded in this document. They cannot reinterpret handoff as execution.

## Human decisions and accountable follow-ups

| Follow-up | Required owner | Closure evidence |
|---|---|---|
| Submit Uber app and validate `history_lite` with real OAuth | Integration/product owner | Redacted consent, token lifecycle, data sample, revocation test, approved privacy policy |
| Apply for Expedia Rapid partnership and define commercial scope | Business/commercial owner | Written approval, contract/profile scope, fee/rate-limit/support matrix |
| Approve Expedia payment, PCI, privacy, and 3DS/SCA model | Security, legal/privacy, payments owners | Architecture decision, provider confirmation, compliance evidence |
| Pass Expedia site review and production fault scenarios | Engineering and operations owners | Review approval, redacted test records, reconciliation/cancellation runbook |
| Confirm Amazon assistant/affiliate permitted use per marketplace | Business/legal owner | Written provider confirmation and active approved tags |
| Ask Zomato for consumer API availability | Business owner | Written scope or explicit denial; current credentials/docs if approved |
| Select another authorized connected-write provider if Expedia misses the gate | Product/operations owner | Written provider authorization, exact-approval and authoritative-outcome contract, production execution/reconciliation evidence, operator runbook |
| Select and approve reminder delivery provider | Product/operations owner | Production delivery/reconciliation evidence and operator runbook for the separate reminder requirement |

## What this research does and does not prove

This record satisfies the evidence-based classification and selection portion of E02. It does **not** constitute credential validation, contractual approval, legal advice, a passed provider site review, or a production test. Those items require accountable humans and provider-issued access.

Accordingly:

- Amazon consumer purchase, Zomato consumer order, and Uber ride request must be declared unavailable and represented as labelled handoff.
- Expedia lodging booking remains disabled until its enablement gates pass.
- Uber connected history remains disabled until real OAuth validation passes.
- Unknown provider outcomes stay `unknown`; retry and handoff never silently become `succeeded`.
- Any material change in provider documentation, approval, terms, regions, payment model, callbacks, or reconciliation behavior requires reopening this decision and downgrading the capability while it is revalidated.
