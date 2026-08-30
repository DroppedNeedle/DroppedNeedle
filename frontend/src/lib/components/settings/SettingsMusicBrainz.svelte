<script lang="ts">
	import { API } from '$lib/constants';
	import { invalidateMusicBrainzProviderQueries } from '$lib/queries/QueryClient';
	import { clearMusicBrainzProviderCaches } from '$lib/utils/albumDetailCache';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { CircleAlert, ExternalLink, Server, ShieldCheck, Users } from 'lucide-svelte';
	import { onDestroy } from 'svelte';

	import {
		COMMUNITY_CONFIRM_LABEL,
		COMMUNITY_CONFIRM_BUTTON_HINT,
		COMMUNITY_RISK_BANNER,
		displayVerifyMessage,
		CLAMPED_WARNING,
		MIRROR_BANNER_LINES,
		MORE_INFO_DISCLOSURES,
		MORE_INFO_SUMMARY,
		MUSICBRAINZ_GUIDE_HREF,
		MUSICBRAINZ_SOURCE_CARDS,
		OFFICIAL_ENDPOINT_URL,
		UNLIMITED_RATE_LABEL,
		type MusicBrainzSourceMode,
		sourceBounds
	} from './musicBrainzSourceCopy';

	type MusicBrainzConnectionSettings = {
		api_url: string;
		rate_limit: number;
		concurrent_searches: number;
		/** Backend sets this when the official-host clamp had to force entered values
		 * down to 1 r/s / 6 concurrent - applied, never refused. */
		clamped_to_official_limits?: boolean;
	};

	type MusicBrainzTestResult = { valid: boolean; message: string };
	type MusicBrainzSettingsForm = ReturnType<
		typeof createSettingsForm<MusicBrainzConnectionSettings>
	> & {
		testResult: MusicBrainzTestResult | null;
	};

	function sourceIdentity(url: string): string {
		const trimmed = url.trim();
		try {
			const parsed = new URL(trimmed);
			const pathname = parsed.pathname.replace(/\/+$/, '') || '/';
			const credentials =
				parsed.username || parsed.password ? `${parsed.username}:${parsed.password}@` : '';
			return `${parsed.protocol}//${credentials}${parsed.host}${pathname}${parsed.search}`;
		} catch {
			return trimmed;
		}
	}

	let loadedSourceIdentity: string | null = null;

	async function handleSaved(data: MusicBrainzConnectionSettings): Promise<void> {
		const nextSourceIdentity = sourceIdentity(data.api_url);
		const sourceChanged =
			loadedSourceIdentity !== null && loadedSourceIdentity !== nextSourceIdentity;
		if (!sourceChanged) return;

		let invalidationSucceeded = false;
		try {
			await invalidateMusicBrainzProviderQueries();
			invalidationSucceeded = true;
		} catch {
			// The server PUT already succeeded; leave the identity unchanged so a later save retries.
		}

		let localCachesCleared = false;
		try {
			localCachesCleared = clearMusicBrainzProviderCaches();
		} catch {
			// Browser cache cleanup is best effort; do not expose the configured source URL.
		}

		if (invalidationSucceeded && localCachesCleared) {
			loadedSourceIdentity = nextSourceIdentity;
		}
	}

	const form = createSettingsForm<MusicBrainzConnectionSettings>({
		loadEndpoint: API.settingsMusicbrainz(),
		saveEndpoint: API.settingsMusicbrainz(),
		testEndpoint: API.settingsMusicbrainzVerify(),
		defaultValue: {
			api_url: OFFICIAL_ENDPOINT_URL,
			rate_limit: 1.0,
			concurrent_searches: 6
		},
		afterSave: handleSaved
	}) as MusicBrainzSettingsForm;

	export async function load() {
		await form.load();
		if (form.data) loadedSourceIdentity = sourceIdentity(form.data.api_url);
	}

	async function save() {
		await form.save();
	}

	async function test() {
		await form.test();
	}

	function resetToDefaults() {
		if (form.data) {
			form.data.api_url = OFFICIAL_ENDPOINT_URL;
			form.data.rate_limit = 1.0;
			form.data.concurrent_searches = 6;
			form.testResult = null;
		}
		selectedMode = 'official';
		communityAcknowledged = false;
	}

	function isOfficialMusicBrainz(url: string): boolean {
		try {
			const parsed = new URL(url.trim());
			const hostname = parsed.hostname.toLowerCase();
			if (hostname !== 'musicbrainz.org' && hostname !== 'www.musicbrainz.org') return false;
			if (parsed.protocol === 'http:') return parsed.port === '' || parsed.port === '80';
			if (parsed.protocol === 'https:') return parsed.port === '' || parsed.port === '443';
			return false;
		} catch {
			return false;
		}
	}

	let isOfficialApi = $derived(form.data ? isOfficialMusicBrainz(form.data.api_url) : true);

	// Explicit card selection sets the mode (OWNER DECISION: the picker auto-highlights
	// Official only for the initial loaded state; a chosen card always wins).
	let selectedMode = $state<MusicBrainzSourceMode>('official');
	let communityAcknowledged = $state(false);
	let modeInitialized = $state(false);

	let effectiveMode = $derived(selectedMode);
	let bounds = $derived(sourceBounds(effectiveMode === 'official'));
	let rateUnlimited = $derived(!isOfficialApi && form.data?.rate_limit === 0);

	let hasPassedTest = $derived(
		form.testResult != null && (form.testResult as MusicBrainzTestResult).valid === true
	);

	let communityNeedsAcknowledgment = $derived(
		effectiveMode === 'community' && !communityAcknowledged
	);
	let saveDisabled = $derived(form.saving || !hasPassedTest || communityNeedsAcknowledgment);
	let saveTooltip = $derived(
		!hasPassedTest
			? 'Test connection before saving'
			: communityNeedsAcknowledgment
				? COMMUNITY_CONFIRM_BUTTON_HINT
				: ''
	);

	let displayedTestMessage = $derived(
		form.testResult
			? displayVerifyMessage((form.testResult as MusicBrainzTestResult).message, isOfficialApi)
			: ''
	);

	const CARD_ICONS = { official: ShieldCheck, mirror: Server, community: Users };

	function selectMode(mode: MusicBrainzSourceMode) {
		selectedMode = mode;
		if (mode === 'official' && form.data) {
			form.data.api_url = OFFICIAL_ENDPOINT_URL;
		}
		if (mode === 'community') {
			communityAcknowledged = false;
		}
	}

	function onCardKeydown(event: KeyboardEvent, mode: MusicBrainzSourceMode) {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			selectMode(mode);
		}
	}

	$effect(() => {
		if (isOfficialApi && form.data) {
			if (form.data.rate_limit > 1.0) form.data.rate_limit = 1.0;
			if (form.data.rate_limit < 0.1) form.data.rate_limit = 1.0;
			if (form.data.concurrent_searches > 6) form.data.concurrent_searches = 6;
		}
	});

	// Consent is tied to the specific server: editing the URL under community mode
	// withdraws the acknowledgment until it is given again.
	$effect(() => {
		if (form.data) {
			const _url = form.data.api_url;
			if (selectedMode === 'community') communityAcknowledged = false;
		}
	});

	// A saved non-official URL loads as the mirror card (the recommended non-official
	// path); switching to community is always an explicit, re-confirmed choice.
	$effect(() => {
		if (!modeInitialized && form.data) {
			modeInitialized = true;
			if (!isOfficialMusicBrainz(form.data.api_url)) selectedMode = 'mirror';
		}
	});

	$effect(() => {
		void load();
	});

	onDestroy(() => form.cleanup());
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">MusicBrainz</h2>
		<p class="text-base-content/70 mb-4">
			Choose where MusicBrainz data comes from, and how fast this server may talk to it. The public
			API is the recommended default; mirrors and community servers are advanced options.
		</p>

		{#if form.loading}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<div class="space-y-4">
				{#if form.data.clamped_to_official_limits}
					<div class="alert alert-warning text-sm" role="status">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{CLAMPED_WARNING}</span>
					</div>
				{/if}

				<div
					role="radiogroup"
					aria-label="MusicBrainz data source"
					class="grid grid-cols-1 md:grid-cols-3 gap-3"
				>
					{#each MUSICBRAINZ_SOURCE_CARDS as card (card.mode)}
						{@const CardIcon = CARD_ICONS[card.mode]}
						{@const isSelected = effectiveMode === card.mode}
						<div
							role="radio"
							aria-checked={isSelected}
							aria-label={card.title}
							tabindex={isSelected ? 0 : -1}
							class="rounded-box border p-4 cursor-pointer space-y-2 focus-visible:outline-2 focus-visible:outline-primary transition-colors {isSelected
								? 'border-primary bg-primary/5'
								: 'border-base-content/15 bg-base-100/60 hover:border-base-content/30'}"
							onclick={() => selectMode(card.mode)}
							onkeydown={(e) => onCardKeydown(e, card.mode)}
						>
							<div class="flex items-center justify-between gap-2">
								<span class="flex items-center gap-2 font-semibold">
									<CardIcon
										class="h-4 w-4 {isSelected ? 'text-primary' : 'text-base-content/60'}"
										aria-hidden="true"
									/>
									{card.title}
								</span>
								{#if card.badge}
									<span class="badge badge-primary badge-sm">{card.badge}</span>
								{/if}
							</div>
							<p class="text-xs text-base-content/60">{card.blurb}</p>
							<details class="text-xs text-base-content/70">
								<summary
									class="cursor-pointer select-none text-base-content/50 hover:text-base-content/80"
								>
									{MORE_INFO_SUMMARY}
								</summary>
								<div class="mt-2 space-y-2">
									{#each MORE_INFO_DISCLOSURES[card.mode] as paragraph (paragraph.slice(0, 40))}
										<p>{paragraph}</p>
									{/each}
									{#if card.mode !== 'official'}
										<a
											href={MUSICBRAINZ_GUIDE_HREF}
											target="_blank"
											rel="noopener noreferrer"
											class="link link-hover inline-flex items-center gap-1 text-primary"
										>
											<ExternalLink class="h-3 w-3" aria-hidden="true" />
											Setup guide
										</a>
									{/if}
								</div>
							</details>
						</div>
					{/each}
				</div>

				{#if effectiveMode !== 'official'}
					<div class="form-control w-full">
						<label class="label" for="mb-api-url">
							<span class="label-text">
								{effectiveMode === 'mirror'
									? 'Mirror API Endpoint URL'
									: 'Community Server API Endpoint URL'}
							</span>
						</label>
						<input
							id="mb-api-url"
							type="text"
							bind:value={form.data.api_url}
							class="input w-full"
							placeholder={effectiveMode === 'mirror'
								? 'http://mirror-host:5000/ws/2'
								: 'https://mb.example.org/ws/2'}
						/>
						<p class="text-xs text-base-content/50 mt-1 ml-1">
							The full URL to the server's MusicBrainz API, including the version path.
						</p>
						{#if effectiveMode === 'mirror'}
							<p class="text-xs text-base-content/50 mt-1 ml-1">
								Test Connection checks the server is alive. Data vintage is documented in the
								<a
									href={MUSICBRAINZ_GUIDE_HREF}
									target="_blank"
									rel="noopener noreferrer"
									class="link link-hover text-primary">setup guide</a
								> (one-line psql command).
							</p>
						{/if}
					</div>
				{/if}

				{#if effectiveMode === 'community'}
					<div class="alert alert-warning text-sm" role="alert">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{COMMUNITY_RISK_BANNER}</span>
					</div>
					<label
						class="label cursor-pointer justify-start gap-3 rounded-box border border-base-content/15 bg-base-100/60 p-4"
					>
						<input
							type="checkbox"
							class="checkbox checkbox-primary"
							bind:checked={communityAcknowledged}
						/>
						<span class="label-text text-sm">{COMMUNITY_CONFIRM_LABEL}</span>
					</label>
				{/if}

				<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
					<div class="form-control w-full">
						<label class="label" for="mb-rate-limit">
							<span class="label-text">Rate Limit (requests/sec)</span>
							{#if rateUnlimited}
								<span class="badge badge-info badge-sm">{UNLIMITED_RATE_LABEL}</span>
							{/if}
						</label>
						<input
							id="mb-rate-limit"
							type="number"
							min={bounds.allowUnlimitedRate ? 0 : 0.1}
							max={bounds.rateMax}
							step="0.1"
							bind:value={form.data.rate_limit}
							class="input w-full"
						/>
						<p class="text-xs text-base-content/50 mt-1 ml-1">
							{#if bounds.allowUnlimitedRate}
								Up to {bounds.rateMax} req/sec on your own or a chosen server. 0 = {UNLIMITED_RATE_LABEL}
								(no client-side limiter). Be polite with servers you do not own.
							{:else}
								Maximum sustained requests per second. The official MusicBrainz limit is 1 req/sec -
								it is clamped here, not refused.
							{/if}
						</p>
					</div>

					<div class="form-control w-full">
						<label class="label" for="mb-concurrent">
							<span class="label-text">Concurrent Searches</span>
						</label>
						<input
							id="mb-concurrent"
							type="number"
							min="1"
							max={bounds.concurrentMax}
							bind:value={form.data.concurrent_searches}
							class="input w-full"
						/>
						<p class="text-xs text-base-content/50 mt-1 ml-1">
							Burst capacity for parallel API requests (official default: 6, max
							{bounds.concurrentMax}).
						</p>
					</div>
				</div>

				{#if !isOfficialApi}
					<div class="alert alert-info text-sm">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<div class="space-y-1">
							{#each MIRROR_BANNER_LINES as line (line.slice(0, 40))}
								<p>{line}</p>
							{/each}
							<a
								href={MUSICBRAINZ_GUIDE_HREF}
								target="_blank"
								rel="noopener noreferrer"
								class="link link-hover inline-flex items-center gap-1 text-primary"
							>
								<ExternalLink class="h-3 w-3" aria-hidden="true" />
								Mirror setup guide
							</a>
						</div>
					</div>
				{/if}

				{#if form.testResult}
					<div
						class="alert"
						class:alert-success={form.testResult.valid}
						class:alert-error={!form.testResult.valid}
					>
						<span>{displayedTestMessage}</span>
					</div>
				{/if}

				{#if form.message}
					<div
						class="alert"
						class:alert-success={form.messageType === 'success'}
						class:alert-error={form.messageType === 'error'}
					>
						<span>{form.message}</span>
					</div>
				{/if}

				<div class="flex justify-between items-center pt-2">
					<button type="button" class="btn btn-outline btn-error btn-sm" onclick={resetToDefaults}>
						Reset to Defaults
					</button>
					<div class="flex gap-2">
						<button
							type="button"
							class="btn btn-ghost"
							onclick={test}
							disabled={form.testing || !form.data.api_url}
						>
							{#if form.testing}
								<span class="loading loading-spinner loading-sm"></span>
							{/if}
							Test Connection
						</button>
						<div class="tooltip" class:tooltip-left={!hasPassedTest} data-tip={saveTooltip}>
							<button type="button" class="btn btn-primary" onclick={save} disabled={saveDisabled}>
								{#if form.saving}
									<span class="loading loading-spinner loading-sm"></span>
								{/if}
								Save Settings
							</button>
						</div>
					</div>
				</div>
			</div>
		{/if}
	</div>
</div>
