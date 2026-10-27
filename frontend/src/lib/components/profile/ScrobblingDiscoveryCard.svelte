<script lang="ts">
	import {
		Radio,
		Music,
		LoaderCircle,
		ExternalLink,
		RadioTower,
		Check,
		Sparkles
	} from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import {
		createConnectListenBrainzMutation,
		createDisconnectMutation,
		createLastFmExchangeSessionMutation,
		createLastFmRequestTokenMutation,
		createSetLastFmCredentialsMutation
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
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		navidromeEnabled: boolean;
	}

	let { navidromeEnabled }: Props = $props();

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);
	const lb = $derived(connections.find((c) => c.service === 'listenbrainz'));
	const lfm = $derived(connections.find((c) => c.service === 'lastfm'));
	// A user's own API key pair is stored as the Last.fm connection before the
	// sign-in finishes; the account name arrives only with the session, so a
	// link without one is not linked yet and still needs Connect and Finish.
	const lfmLinked = $derived(Boolean(lfm?.username));

	const prefsQuery = getScrobblePreferencesQuery();
	const prefs = $derived(prefsQuery.data);

	const requestTokenMutation = createLastFmRequestTokenMutation();
	const exchangeSessionMutation = createLastFmExchangeSessionMutation();
	const setLastFmCredentialsMutation = createSetLastFmCredentialsMutation();
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
	let lfmCredentialsOpen = $state(false);
	let lfmApiKey = $state('');
	let lfmSecret = $state('');

	// optimistic mirror so toggles feel instant; re-synced whenever the query settles
	let scrobbleLastfm = $state(false);
	let scrobbleListenbrainz = $state(false);
	let navidromeHandlesExternalScrobbles = $state(true);
	let primarySource = $state<MusicSource>('listenbrainz');
	let nowPlayingVisibility = $state<NowPlayingVisibility>('full');
	let autoRequestPersonalMix = $state(false);
	let mixRefreshMessage = $state<string | null>(null);
	const autoRequestState = $derived(prefs?.auto_request_state ?? 'none');
	const autoRequestNote = $derived.by(() => {
		if (!lb) return 'Link ListenBrainz above to enable.';
		if (autoRequestPersonalMix && autoRequestState === 'pending')
			return 'Waiting for an admin to approve auto-requests.';
		if (autoRequestState === 'rejected' || autoRequestState === 'revoked')
			return 'An admin declined auto-requests for your mix.';
		return undefined;
	});
	const visibilityId = $props.id();
	$effect(() => {
		if (prefs) {
			scrobbleLastfm = prefs.scrobble_to_lastfm ?? false;
			scrobbleListenbrainz = prefs.scrobble_to_listenbrainz ?? false;
			navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles ?? true;
			primarySource = (prefs.primary_music_source as MusicSource) ?? 'listenbrainz';
			nowPlayingVisibility = (prefs.now_playing_visibility as NowPlayingVisibility) ?? 'full';
			autoRequestPersonalMix = prefs.auto_request_personal_mix ?? false;
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
			// 409 means there is no Last.fm app key to link with: the admin has not
			// saved the server's key pair and this user has not saved their own.
			if (e instanceof ApiError && e.status === 409) {
				lfmCredentialsOpen = true;
				return;
			}
			lfmError = errorMessage(e, 'Could not start Last.fm sign-in.');
		}
	}

	async function saveLastFmCredentials() {
		lfmError = null;
		try {
			await setLastFmCredentialsMutation.mutateAsync({
				api_key: lfmApiKey.trim(),
				shared_secret: lfmSecret.trim()
			});
			lfmCredentialsOpen = false;
			lfmApiKey = '';
			lfmSecret = '';
			await startLastFm();
		} catch (e) {
			lfmError = errorMessage(e, 'Could not save your Last.fm credentials.');
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
				scrobbleLastfm = prefs.scrobble_to_lastfm ?? false;
				scrobbleListenbrainz = prefs.scrobble_to_listenbrainz ?? false;
				navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles ?? true;
				primarySource = (prefs.primary_music_source as MusicSource) ?? 'listenbrainz';
				nowPlayingVisibility = (prefs.now_playing_visibility as NowPlayingVisibility) ?? 'full';
				autoRequestPersonalMix = prefs.auto_request_personal_mix ?? false;
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

{#snippet serviceRow(
	Icon: typeof Music,
	tint: string,
	name: string,
	username: string | null | undefined,
	linked: boolean
)}
	<div class="flex min-w-0 items-center gap-3">
		<div class="flex h-10 w-10 items-center justify-center rounded-xl ring-1 {tint}">
			<Icon class="h-[1.15rem] w-[1.15rem]" />
		</div>
		<div class="min-w-0">
			<div class="flex items-center gap-2">
				<span class="text-sm font-semibold">{name}</span>
				<span class="status {linked ? 'status-success' : 'status-error'} status-sm"></span>
			</div>
			{#if linked}
				<p class="truncate text-xs text-base-content/50">@{username || 'linked'}</p>
			{:else}
				<p class="text-xs text-base-content/30">Not connected</p>
			{/if}
		</div>
	</div>
{/snippet}

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<RadioTower class="h-4 w-4 text-accent" />
		Scrobbling &amp; Discovery
	</h2>

	<Card variant="bordered" padding="md">
		<div class="space-y-3">
			{#if loading}
				<div class="flex items-center justify-center py-10">
					<LoaderCircle class="h-5 w-5 animate-spin motion-keep text-base-content/40" />
				</div>
			{:else}
				<div>
					<Card variant="bordered" padding="sm">
						<div class="flex items-center justify-between gap-3">
							{@render serviceRow(
								Music,
								'bg-listenbrainz/10 text-listenbrainz ring-listenbrainz/20',
								'ListenBrainz',
								lb?.username,
								Boolean(lb)
							)}
							<div class="shrink-0">
								{#if lb}
									<Button
										variant="ghost"
										size="xs"
										pill
										onclick={() => disconnect('listenbrainz')}
										disabled={disconnectMutation.isPending}
									>
										Disconnect
									</Button>
								{:else}
									<Button
										variant="listenbrainz"
										size="xs"
										pill
										onclick={() => (lbFormOpen = !lbFormOpen)}
									>
										Connect
									</Button>
								{/if}
							</div>
						</div>
					</Card>

					{#if !lb && lbFormOpen}
						<Card variant="bordered" padding="sm" class="mt-2 animate-fade-in-up">
							<div class="space-y-2">
								<p class="text-xs text-base-content/60">
									Paste your ListenBrainz user token and username to link your account.
								</p>
								<Input
									type="password"
									label="ListenBrainz user token"
									size="sm"
									class="w-full"
									placeholder="User token"
									autocomplete="off"
									bind:value={lbToken}
								/>
								<Input
									label="ListenBrainz username"
									size="sm"
									class="w-full"
									placeholder="ListenBrainz username"
									autocomplete="off"
									bind:value={lbUsername}
								/>
								{#if lbError}
									<p class="text-xs text-error">{lbError}</p>
								{/if}
								<div class="flex justify-end gap-2">
									<Button
										variant="ghost"
										size="xs"
										pill
										onclick={() => {
											lbFormOpen = false;
											lbError = null;
										}}
									>
										Cancel
									</Button>
									<Button
										variant="primary"
										size="xs"
										pill
										loading={connectLbMutation.isPending}
										disabled={!lbToken.trim() || !lbUsername.trim()}
										onclick={connectListenBrainz}
									>
										Link account
									</Button>
								</div>
							</div>
						</Card>
					{/if}
				</div>

				<div>
					<Card variant="bordered" padding="sm">
						<div class="flex items-center justify-between gap-3">
							{@render serviceRow(
								Radio,
								'bg-lastfm/10 text-lastfm ring-lastfm/20',
								'Last.fm',
								lfm?.username,
								lfmLinked
							)}
							<div class="flex shrink-0 items-center gap-2">
								{#if lfm}
									<Button
										variant="ghost"
										size="xs"
										pill
										onclick={() => disconnect('lastfm')}
										disabled={disconnectMutation.isPending}
									>
										Disconnect
									</Button>
								{/if}
								{#if !lfmLinked && lfmPendingToken}
									<div class="flex items-center gap-2">
										<Button variant="ghost" size="xs" pill onclick={cancelLastFm}>Cancel</Button>
										<Button
											variant="primary"
											size="xs"
											pill
											icon={Check}
											loading={exchangeSessionMutation.isPending}
											onclick={finishLastFm}
										>
											Finish
										</Button>
									</div>
								{:else if !lfmLinked}
									<Button
										variant="lastfm"
										size="xs"
										pill
										icon={ExternalLink}
										loading={requestTokenMutation.isPending}
										onclick={startLastFm}
									>
										Connect
									</Button>
								{/if}
							</div>
						</div>
					</Card>
					{#if !lfmLinked && lfmCredentialsOpen}
						<Card variant="bordered" padding="sm" class="mt-2 animate-fade-in-up">
							<div class="space-y-2">
								<p class="text-xs text-base-content/60">
									Enter the API key and shared secret of your own
									<a
										href="https://www.last.fm/api/account/create"
										target="_blank"
										rel="noopener noreferrer"
										class="link">Last.fm API account</a
									>, then connect.
								</p>
								<Input
									label="Last.fm API key"
									size="sm"
									class="w-full"
									placeholder="API key"
									autocomplete="off"
									bind:value={lfmApiKey}
								/>
								<Input
									type="password"
									label="Last.fm shared secret"
									size="sm"
									class="w-full"
									placeholder="Shared secret"
									autocomplete="off"
									bind:value={lfmSecret}
								/>
								<div class="flex justify-end gap-2">
									<Button
										variant="ghost"
										size="xs"
										pill
										onclick={() => (lfmCredentialsOpen = false)}
									>
										Cancel
									</Button>
									<Button
										variant="primary"
										size="xs"
										pill
										loading={setLastFmCredentialsMutation.isPending}
										disabled={!lfmApiKey.trim() || !lfmSecret.trim()}
										onclick={saveLastFmCredentials}
									>
										Save and connect
									</Button>
								</div>
							</div>
						</Card>
					{/if}
					{#if lfmPendingToken && !lfmLinked}
						<p class="mt-2 px-1 text-xs text-base-content/60 animate-fade-in-up">
							Approve DroppedNeedle in the Last.fm window that opened, then choose Finish.
						</p>
					{/if}
					{#if lfmError}
						<p class="mt-2 px-1 text-xs text-error">{lfmError}</p>
					{/if}
				</div>

				<div class="section-divider-glow my-1"></div>

				<div class="space-y-3 px-1">
					<ToggleRow
						label="Scrobble to Last.fm"
						description={lfmLinked ? undefined : 'Link Last.fm above to scrobble.'}
						disabled={!lfmLinked || updatePrefsMutation.isPending}
						bind:checked={
							() => scrobbleLastfm,
							(checked) => {
								scrobbleLastfm = checked;
								void savePrefs({ scrobble_to_lastfm: checked });
							}
						}
					/>

					<ToggleRow
						label="Scrobble to ListenBrainz"
						description={lb ? undefined : 'Link ListenBrainz above to scrobble.'}
						disabled={!lb || updatePrefsMutation.isPending}
						bind:checked={
							() => scrobbleListenbrainz,
							(checked) => {
								scrobbleListenbrainz = checked;
								void savePrefs({ scrobble_to_listenbrainz: checked });
							}
						}
					/>

					{#if navidromeEnabled}
						<ToggleRow
							label="Let Navidrome handle Last.fm and ListenBrainz"
							description="Keep this on while Navidrome sends plays to those accounts. To use DroppedNeedle instead, first disable external scrobbles for the DroppedNeedle player in Navidrome. Listening history is recorded either way."
							disabled={updatePrefsMutation.isPending}
							bind:checked={
								() => navidromeHandlesExternalScrobbles,
								(checked) => {
									navidromeHandlesExternalScrobbles = checked;
									void savePrefs({ navidrome_handles_external_scrobbles: checked });
								}
							}
						/>
					{/if}
				</div>

				<div class="section-divider-glow my-1"></div>

				<div class="flex flex-wrap items-center justify-between gap-3 px-1">
					<div>
						<span class="text-sm font-medium">Primary source</span>
						<p class="text-xs text-base-content/40">Powers your Home &amp; Discover by default.</p>
					</div>
					<div
						class="inline-flex gap-1 rounded-full bg-base-300/50 p-1"
						role="group"
						aria-label="Primary source"
					>
						{#each SOURCES as opt (opt.value)}
							<Button
								variant={primarySource === opt.value ? 'primary' : 'ghost'}
								size="xs"
								pill
								aria-pressed={primarySource === opt.value}
								disabled={updatePrefsMutation.isPending}
								onclick={() => selectSource(opt.value)}
							>
								{opt.label}
							</Button>
						{/each}
					</div>
				</div>

				<div class="section-divider-glow my-1"></div>

				<div class="flex flex-wrap items-center justify-between gap-3 px-1">
					<div>
						<label for={visibilityId} class="text-sm font-medium">Now Playing visibility</label>
						<p class="text-xs text-base-content/40">
							How your listening appears to others on the home page.
						</p>
					</div>
					<select
						id={visibilityId}
						class="select select-sm w-full max-w-[16rem] text-xs"
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
					<ToggleRow
						label="Auto-request missing music from Your Weekly Mix"
						description={autoRequestNote}
						disabled={!lb || updatePrefsMutation.isPending}
						bind:checked={
							() => autoRequestPersonalMix,
							(checked) => {
								autoRequestPersonalMix = checked;
								void savePrefs({ auto_request_personal_mix: checked });
							}
						}
					/>
					<div class="flex items-center gap-3">
						<Button
							variant="ghost"
							size="xs"
							pill
							loading={refreshMixMutation.isPending}
							disabled={!lb}
							onclick={refreshPersonalMix}
						>
							Refresh my mix
						</Button>
						{#if mixRefreshMessage}
							<p class="grow-0 text-xs text-base-content/50">{mixRefreshMessage}</p>
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</Card>
</section>
