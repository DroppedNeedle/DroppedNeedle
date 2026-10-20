<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { getApiUrl } from '$lib/api/api-utils';
	import { api } from '$lib/api/client';
	import { SPOTIFY_ENDPOINTS } from '$lib/queries/spotify/endpoints';
	import {
		createSaveSpotifySettingsMutation,
		getSpotifySettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { Copy, ExternalLink } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	let redirectUri = $state('');
	let copied = $state(false);

	function copyRedirectUri() {
		navigator.clipboard
			.writeText(redirectUri)
			.then(() => {
				copied = true;
				setTimeout(() => (copied = false), 2000);
			})
			.catch(() => {
				/* clipboard unavailable (insecure context / permission denied) - ignore */
			});
	}

	const section = createSettingsSection(
		getSpotifySettingsQuery(),
		createSaveSpotifySettingsMutation()
	);

	// The server builds the address from the request when no origin is saved,
	// so behind a proxy it does not know about it can name the wrong one.
	const addressIsElsewhere = $derived.by(() => {
		if (!redirectUri || section.saved?.spotify_redirect_origin.trim()) return false;
		try {
			return new URL(redirectUri).origin !== window.location.origin;
		} catch {
			return false;
		}
	});

	async function loadRedirectUri() {
		try {
			const data = await api.global.v3.GET(SPOTIFY_ENDPOINTS.redirectUri());
			redirectUri = data.redirect_uri;
		} catch {
			redirectUri = new URL(
				getApiUrl(SPOTIFY_ENDPOINTS.authCallback()),
				window.location.origin
			).toString();
		}
	}

	// The address follows the saved origin: read it again whenever that changes.
	$effect(() => {
		if (section.saved === undefined) return;
		void section.saved.spotify_redirect_origin;
		void loadRedirectUri();
	});
</script>

<Card variant="bordered">
	{#snippet header()}
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-spotify/10 text-spotify ring-1 ring-spotify/20"
			>
				<svg class="h-5 w-5" viewBox="0 0 24 24" fill="currentColor">
					<path
						d="M12 2C6.477 2 2 6.477 2 12s4.477 10 10 10 10-4.477 10-10S17.523 2 12 2zm4.586 14.424a.622.622 0 01-.857.207c-2.348-1.435-5.304-1.76-8.785-.964a.622.622 0 11-.277-1.215c3.809-.87 7.076-.496 9.712 1.115a.622.622 0 01.207.857zm1.223-2.723a.778.778 0 01-1.07.257c-2.687-1.652-6.785-2.131-9.965-1.166a.778.778 0 01-.966-.519.778.778 0 01.519-.966c3.632-1.102 8.147-.568 11.225 1.324a.778.778 0 01.257 1.07zm.105-2.835C14.692 8.95 9.375 8.775 6.297 9.71a.934.934 0 11-.543-1.787c3.533-1.073 9.407-.866 13.115 1.337a.934.934 0 01-1.955.606z"
					/>
				</svg>
			</div>
			<div>
				<CardTitle>Spotify</CardTitle>
				<p class="text-sm text-base-content/60">App credentials for the whole instance.</p>
			</div>
		</div>
	{/snippet}
	<div class="mt-2 flex flex-col gap-4">
		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			These are the shared app credentials for one registered Spotify application. Each user links
			<span class="font-medium">their own</span> Spotify account from their
			<a href={withBasePath('/profile')} class="link link-primary">profile</a> to import their personal
			playlists.
		</div>

		<div class="rounded-xl border border-warning/20 bg-warning/5 p-3 text-sm text-base-content/70">
			<p class="font-medium text-warning/80">Development mode restriction</p>
			<p class="mt-1">
				Spotify apps in development mode only allow up to 5 explicitly allowlisted users. Each user
				who wants to import playlists must be added by email in your
				<a
					href="https://developer.spotify.com/dashboard"
					target="_blank"
					rel="noopener noreferrer"
					class="link link-primary">Spotify app dashboard</a
				>
				under <span class="font-medium">User Management</span>.
			</p>
		</div>

		<QueryBoundary query={section} what="the Spotify settings">
			{#snippet children(draft)}
				<div class="flex flex-col gap-4">
					<TextField
						label="Client ID"
						placeholder="Spotify Client ID"
						bind:value={draft.client_id}
					/>

					<SecretField
						label="Client Secret"
						mask={SECRET_MASK}
						placeholder="Spotify Client Secret"
						bind:value={draft.client_secret}
					/>

					<TextField
						label="Redirect Origin (optional)"
						placeholder="https://music.example.com"
						help="Leave this empty and DroppedNeedle uses the address it works out for itself. That is right when you reach it directly, or through a reverse proxy that is listed in TRUSTED_PROXY_IPS and sends X-Forwarded-Proto. Fill it in (no path) when the redirect URI below does not start with the address in your browser's address bar."
						bind:value={draft.spotify_redirect_origin}
					/>

					<ToggleRow label="Enable Spotify integration" bind:checked={draft.enabled} />

					<Card variant="bordered" padding="sm">
						<div>
							<p class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-base-content/50">
								Redirect URI: add this in your Spotify app dashboard
							</p>
							<div class="flex items-center gap-2">
								<code
									class="flex-1 truncate rounded-lg bg-base-100/60 px-2.5 py-1.5 text-xs text-base-content/80"
								>
									{redirectUri}
								</code>
								<Button
									variant="ghost"
									size="xs"
									pill
									icon={copied ? undefined : Copy}
									class="shrink-0"
									aria-label="Copy redirect URI"
									onclick={copyRedirectUri}
								>
									{#if copied}Copied!{/if}
								</Button>
							</div>
							{#if addressIsElsewhere}
								<p class="mt-2 text-xs text-warning">
									This is not the address you are on. Put {window.location.origin} in Redirect Origin
									and save.
								</p>
							{/if}
						</div>
					</Card>

					<a
						href="https://developer.spotify.com/dashboard"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" /> Create a Spotify app to get Client ID + Secret
					</a>

					<SaveBar {...section.bar} saveLabel="Save credentials" />
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
