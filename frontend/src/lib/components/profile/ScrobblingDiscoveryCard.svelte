<script lang="ts">
	import { Radio, Music, Loader2, ExternalLink, RadioTower, Check, Sparkles } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import {
		createConnectListenBrainzMutation,
		createDisconnectMutation,
		createLastFmExchangeSessionMutation,
		createLastFmRequestTokenMutation
	} from '$lib/queries/connections/ConnectionsMutations.svelte';
	import { getScrobblePreferencesQuery } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
	import {
		createRefreshPersonalMixMutation,
		createUpdateScrobblePreferencesMutation
	} from '$lib/queries/scrobble-preferences/ScrobblePreferencesMutations.svelte';
	import type {
		NowPlayingVisibility,
		ScrobblePreferencesUpdate
	} from '$lib/queries/scrobble-preferences/types';
	import type { MusicSource } from '$lib/stores/musicSource';
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';

	interface Props {
		navidromeEnabled: boolean;
	}

	let { navidromeEnabled }: Props = $props();

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);
	const lb = $derived(connections.find((c) => c.service === 'listenbrainz'));
	const lfm = $derived(connections.find((c) => c.service === 'lastfm'));

	const prefsQuery = getScrobblePreferencesQuery();
	const prefs = $derived(prefsQuery.data);

	const requestTokenMutation = createLastFmRequestTokenMutation();
	const exchangeSessionMutation = createLastFmExchangeSessionMutation();
	const connectLbMutation = createConnectListenBrainzMutation();
	const disconnectMutation = createDisconnectMutation();
	const updatePrefsMutation = createUpdateScrobblePreferencesMutation();
	const refreshMixMutation = createRefreshPersonalMixMutation();

	const loading = $derived(connectionsQuery.isPending || prefsQuery.isPending);

	const SOURCES: { value: MusicSource; label: string }[] = [
		{ value: 'listenbrainz', label: 'ListenBrainz' },
		{ value: 'lastfm', label: 'Last.fm' }
	];

	let lbFormOpen = $state(false);
	let lbToken = $state('');
	let lbUsername = $state('');
	let lbError = $state<string | null>(null);

	let lfmPendingToken = $state<string | null>(null);
	let lfmError = $state<string | null>(null);

	// optimistic mirror so toggles feel instant; re-synced whenever the query settles
	let scrobbleLastfm = $state(false);
	let scrobbleListenbrainz = $state(false);
	let navidromeHandlesExternalScrobbles = $state(true);
	let primarySource = $state<MusicSource>('listenbrainz');
	let nowPlayingVisibility = $state<NowPlayingVisibility>('full');
	let autoRequestPersonalMix = $state(false);
	let mixRefreshMessage = $state<string | null>(null);
	const autoRequestState = $derived(prefs?.auto_request_state ?? 'none');
	$effect(() => {
		if (prefs) {
			scrobbleLastfm = prefs.scrobble_to_lastfm;
			scrobbleListenbrainz = prefs.scrobble_to_listenbrainz;
			navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles;
			primarySource = (prefs.primary_music_source as MusicSource) ?? 'listenbrainz';
			nowPlayingVisibility = (prefs.now_playing_visibility as NowPlayingVisibility) ?? 'full';
			autoRequestPersonalMix = prefs.auto_request_personal_mix;
		}
	});

	function errorMessage(e: unknown, fallback: string): string {
		return e instanceof ApiError ? e.message : fallback;
	}

	async function connectListenBrainz() {
		lbError = null;
		try {
			await connectLbMutation.mutateAsync({
				user_token: lbToken.trim(),
				username: lbUsername.trim()
			});
			lbFormOpen = false;
			lbToken = '';
			lbUsername = '';
		} catch (e) {
			lbError = errorMessage(e, 'Could not verify that token.');
		}
	}

	async function startLastFm() {
		lfmError = null;
		try {
			const data = await requestTokenMutation.mutateAsync();
			lfmPendingToken = data.token;
			window.open(data.auth_url, '_blank', 'popup=yes,noopener,noreferrer');
		} catch (e) {
			lfmError = errorMessage(e, 'Could not start Last.fm sign-in.');
		}
	}

	async function finishLastFm() {
		if (!lfmPendingToken) return;
		lfmError = null;
		try {
			await exchangeSessionMutation.mutateAsync(lfmPendingToken);
			lfmPendingToken = null;
		} catch (e) {
			lfmError = errorMessage(e, "Authorization isn't complete yet - approve it, then retry.");
		}
	}

	function cancelLastFm() {
		lfmPendingToken = null;
		lfmError = null;
	}

	async function disconnect(service: string) {
		await disconnectMutation.mutateAsync(service);
		if (service === 'lastfm') lfmPendingToken = null;
		await scrobbleManager.refreshSettings();
	}

	async function savePrefs(update: ScrobblePreferencesUpdate) {
		try {
			await updatePrefsMutation.mutateAsync(update);
			await scrobbleManager.refreshSettings();
		} catch {
			if (prefs) {
				scrobbleLastfm = prefs.scrobble_to_lastfm;
				scrobbleListenbrainz = prefs.scrobble_to_listenbrainz;
				navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles;
				primarySource = (prefs.primary_music_source as MusicSource) ?? 'listenbrainz';
				nowPlayingVisibility = (prefs.now_playing_visibility as NowPlayingVisibility) ?? 'full';
				autoRequestPersonalMix = prefs.auto_request_personal_mix;
			}
		}
	}

	async function selectSource(src: MusicSource) {
		if (src === primarySource) return;
		primarySource = src;
		await savePrefs({ primary_music_source: src });
	}

	async function refreshPersonalMix() {
		mixRefreshMessage = null;
		try {
			const result = await refreshMixMutation.mutateAsync();
			// the build runs in the background; completion arrives as a toast via
			// the personal_mix_refreshed SSE event
			mixRefreshMessage =
				result.status === 'already_running'
					? 'A refresh is already running - hang tight.'
					: "Refreshing in the background - we'll let you know when it's ready.";
		} catch (e) {
			mixRefreshMessage = errorMessage(e, "Couldn't refresh Your Weekly Mix.");
		}
	}
</script>

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<RadioTower class="h-4 w-4 text-accent" />
		Scrobbling &amp; Discovery
	</h2>

	<div
		class="glow-primary-soft space-y-3 rounded-2xl border border-base-300/50 bg-base-200/40 p-4 backdrop-blur-sm sm:p-5"
	>
		{#if loading}
			<div class="flex items-center justify-center py-10">
				<Loader2 class="h-5 w-5 animate-spin text-base-content/40" />
			</div>
		{:else}
			<div>
				<div
					class="crate-card flex items-center justify-between gap-3 rounded-xl border border-base-300/40 bg-base-300/20 p-3"
				>
					<div class="flex min-w-0 items-center gap-3">
						<div
							class="flex h-10 w-10 items-center justify-center rounded-xl bg-orange-500/10 text-orange-400 ring-1 ring-orange-500/20"
						>
							<Music class="h-[1.15rem] w-[1.15rem]" />
						</div>
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<span class="text-sm font-semibold">ListenBrainz</span>
								<span class="status {lb ? 'status-success' : 'status-error'} status-sm"></span>
							</div>
							{#if lb}
								<p class="truncate text-xs text-base-content/50">@{lb.username || 'linked'}</p>
							{:else}
								<p class="text-xs text-base-content/30">Not connected</p>
							{/if}
						</div>
					</div>
					<div class="shrink-0">
						{#if lb}
							<button
								type="button"
								class="btn btn-ghost btn-xs rounded-full"
								onclick={() => disconnect('listenbrainz')}
								disabled={disconnectMutation.isPending}
							>
								Disconnect
							</button>
						{:else}
							<button
								type="button"
								class="btn btn-listenbrainz btn-xs gap-1 rounded-full px-3 shadow-sm transition-transform hover:scale-[1.03]"
								onclick={() => (lbFormOpen = !lbFormOpen)}
							>
								Connect
							</button>
						{/if}
					</div>
				</div>

				{#if !lb && lbFormOpen}
					<div
						class="mt-2 space-y-2 rounded-xl border border-base-300/40 bg-base-100/40 p-3 animate-fade-in-up"
					>
						<p class="text-xs text-base-content/60">
							Paste your ListenBrainz user token and username to link your account.
						</p>
						<input
							type="password"
							class="input input-sm input-soft w-full"
							placeholder="User token"
							bind:value={lbToken}
							autocomplete="off"
						/>
						<input
							type="text"
							class="input input-sm input-soft w-full"
							placeholder="ListenBrainz username"
							bind:value={lbUsername}
							autocomplete="off"
						/>
						{#if lbError}
							<p class="text-xs text-error">{lbError}</p>
						{/if}
						<div class="flex justify-end gap-2">
							<button
								type="button"
								class="btn btn-ghost btn-xs rounded-full"
								onclick={() => {
									lbFormOpen = false;
									lbError = null;
								}}
							>
								Cancel
							</button>
							<button
								type="button"
								class="btn btn-primary btn-xs gap-1 rounded-full"
								onclick={connectListenBrainz}
								disabled={connectLbMutation.isPending || !lbToken.trim() || !lbUsername.trim()}
							>
								{#if connectLbMutation.isPending}
									<Loader2 class="h-3.5 w-3.5 animate-spin" />
								{/if}
								Link account
							</button>
						</div>
					</div>
				{/if}
			</div>

			<div>
				<div
					class="crate-card flex items-center justify-between gap-3 rounded-xl border border-base-300/40 bg-base-300/20 p-3"
				>
					<div class="flex min-w-0 items-center gap-3">
						<div
							class="flex h-10 w-10 items-center justify-center rounded-xl bg-red-500/10 text-red-400 ring-1 ring-red-500/20"
						>
							<Radio class="h-[1.15rem] w-[1.15rem]" />
						</div>
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<span class="text-sm font-semibold">Last.fm</span>
								<span class="status {lfm ? 'status-success' : 'status-error'} status-sm"></span>
							</div>
							{#if lfm}
								<p class="truncate text-xs text-base-content/50">@{lfm.username || 'linked'}</p>
							{:else}
								<p class="text-xs text-base-content/30">Not connected</p>
							{/if}
						</div>
					</div>
					<div class="shrink-0">
						{#if lfm}
							<button
								type="button"
								class="btn btn-ghost btn-xs rounded-full"
								onclick={() => disconnect('lastfm')}
								disabled={disconnectMutation.isPending}
							>
								Disconnect
							</button>
						{:else if lfmPendingToken}
							<div class="flex items-center gap-2">
								<button
									type="button"
									class="btn btn-ghost btn-xs rounded-full"
									onclick={cancelLastFm}
								>
									Cancel
								</button>
								<button
									type="button"
									class="btn btn-primary btn-xs gap-1 rounded-full"
									onclick={finishLastFm}
									disabled={exchangeSessionMutation.isPending}
								>
									{#if exchangeSessionMutation.isPending}
										<Loader2 class="h-3.5 w-3.5 animate-spin" />
									{:else}
										<Check class="h-3.5 w-3.5" />
									{/if}
									Finish
								</button>
							</div>
						{:else}
							<button
								type="button"
								class="btn btn-lastfm btn-xs gap-1 rounded-full px-3 shadow-sm transition-transform hover:scale-[1.03]"
								onclick={startLastFm}
								disabled={requestTokenMutation.isPending}
							>
								{#if requestTokenMutation.isPending}
									<Loader2 class="h-3.5 w-3.5 animate-spin" />
								{:else}
									<ExternalLink class="h-3.5 w-3.5" />
								{/if}
								Connect
							</button>
						{/if}
					</div>
				</div>
				{#if lfmPendingToken && !lfm}
					<p class="mt-2 px-1 text-xs text-base-content/60 animate-fade-in-up">
						Approve DroppedNeedle in the Last.fm window that opened, then choose Finish.
					</p>
				{/if}
				{#if lfmError}
					<p class="mt-2 px-1 text-xs text-error">{lfmError}</p>
				{/if}
			</div>

			<div class="section-divider-glow my-1"></div>

			<div class="space-y-1">
				<label
					class="flex cursor-pointer items-center justify-between gap-4 rounded-lg px-1 py-2 transition-colors hover:bg-base-300/20"
				>
					<div>
						<span class="text-sm font-medium">Scrobble to Last.fm</span>
						{#if !lfm}
							<p class="text-xs text-base-content/40">Link Last.fm above to scrobble.</p>
						{/if}
					</div>
					<input
						type="checkbox"
						class="toggle toggle-primary toggle-sm"
						bind:checked={scrobbleLastfm}
						disabled={!lfm || updatePrefsMutation.isPending}
						onchange={() => savePrefs({ scrobble_to_lastfm: scrobbleLastfm })}
					/>
				</label>

				<label
					class="flex cursor-pointer items-center justify-between gap-4 rounded-lg px-1 py-2 transition-colors hover:bg-base-300/20"
				>
					<div>
						<span class="text-sm font-medium">Scrobble to ListenBrainz</span>
						{#if !lb}
							<p class="text-xs text-base-content/40">Link ListenBrainz above to scrobble.</p>
						{/if}
					</div>
					<input
						type="checkbox"
						class="toggle toggle-primary toggle-sm"
						bind:checked={scrobbleListenbrainz}
						disabled={!lb || updatePrefsMutation.isPending}
						onchange={() => savePrefs({ scrobble_to_listenbrainz: scrobbleListenbrainz })}
					/>
				</label>

				{#if navidromeEnabled}
					<label
						class="flex cursor-pointer items-center justify-between gap-4 rounded-lg px-1 py-2 transition-colors hover:bg-base-300/20"
					>
						<div>
							<span class="text-sm font-medium">Let Navidrome handle Last.fm and ListenBrainz</span>
							<p class="text-xs text-base-content/40">
								Keep this on while Navidrome sends plays to those accounts. To use DroppedNeedle
								instead, first disable external scrobbles for the DroppedNeedle player in Navidrome.
								Listening history is recorded either way.
							</p>
						</div>
						<input
							type="checkbox"
							class="toggle toggle-primary toggle-sm"
							bind:checked={navidromeHandlesExternalScrobbles}
							disabled={updatePrefsMutation.isPending}
							onchange={() =>
								savePrefs({
									navidrome_handles_external_scrobbles: navidromeHandlesExternalScrobbles
								})}
						/>
					</label>
				{/if}
			</div>

			<div class="section-divider-glow my-1"></div>

			<div class="flex flex-wrap items-center justify-between gap-3 px-1">
				<div>
					<span class="text-sm font-medium">Primary source</span>
					<p class="text-xs text-base-content/40">Powers your Home &amp; Discover by default.</p>
				</div>
				<div class="inline-flex rounded-full bg-base-300/50 p-1">
					{#each SOURCES as opt (opt.value)}
						<button
							type="button"
							class="rounded-full px-3.5 py-1 text-xs font-semibold transition-all {primarySource ===
							opt.value
								? 'bg-primary text-primary-content shadow'
								: 'text-base-content/55 hover:text-base-content'}"
							disabled={updatePrefsMutation.isPending}
							onclick={() => selectSource(opt.value)}
						>
							{opt.label}
						</button>
					{/each}
				</div>
			</div>

			<div class="section-divider-glow my-1"></div>

			<div class="flex flex-wrap items-center justify-between gap-3 px-1">
				<div>
					<span class="text-sm font-medium">Now Playing visibility</span>
					<p class="text-xs text-base-content/40">
						How your listening appears to others on the home page.
					</p>
				</div>
				<select
					class="select select-bordered select-sm w-full max-w-[16rem] text-xs"
					bind:value={nowPlayingVisibility}
					disabled={updatePrefsMutation.isPending}
					onchange={() => savePrefs({ now_playing_visibility: nowPlayingVisibility })}
				>
					<option value="full">Show what I'm playing</option>
					<option value="track_hidden">Show I'm listening, hide the track</option>
					<option value="offline">Don't show my activity</option>
				</select>
			</div>

			<div class="section-divider-glow my-1"></div>

			<div class="space-y-2 px-1">
				<div class="flex items-center gap-2">
					<Sparkles class="h-4 w-4 text-accent" />
					<span class="text-sm font-semibold">Your Weekly Mix</span>
				</div>
				<p class="text-xs text-base-content/40">
					A playlist built from your ListenBrainz listening history, refreshed weekly.
				</p>
				<label
					class="flex cursor-pointer items-center justify-between gap-4 rounded-lg py-2 transition-colors hover:bg-base-300/20"
				>
					<div>
						<span class="text-sm font-medium">Auto-request missing music from Your Weekly Mix</span>
						{#if !lb}
							<p class="text-xs text-base-content/40">Link ListenBrainz above to enable.</p>
						{:else if autoRequestPersonalMix && autoRequestState === 'pending'}
							<p class="text-xs text-warning/80">Waiting for an admin to approve auto-requests.</p>
						{:else if autoRequestState === 'rejected' || autoRequestState === 'revoked'}
							<p class="text-xs text-base-content/40">
								An admin declined auto-requests for your mix.
							</p>
						{/if}
					</div>
					<input
						type="checkbox"
						class="toggle toggle-primary toggle-sm"
						bind:checked={autoRequestPersonalMix}
						disabled={!lb || updatePrefsMutation.isPending}
						onchange={() => savePrefs({ auto_request_personal_mix: autoRequestPersonalMix })}
					/>
				</label>
				<div class="flex items-center gap-3">
					<button
						type="button"
						class="btn btn-ghost btn-xs gap-1 rounded-full"
						onclick={refreshPersonalMix}
						disabled={!lb || refreshMixMutation.isPending}
					>
						{#if refreshMixMutation.isPending}
							<Loader2 class="h-3.5 w-3.5 animate-spin" />
						{/if}
						Refresh my mix
					</button>
					{#if mixRefreshMessage}
						<p class="text-xs text-base-content/50">{mixRefreshMessage}</p>
					{/if}
				</div>
			</div>
		{/if}
	</div>
</section>
