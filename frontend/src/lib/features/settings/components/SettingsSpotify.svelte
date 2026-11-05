<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import { v3 } from '$lib/api/v3/endpoint';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { getApiUrl } from '$lib/api/api-utils';
	import {
		createSaveSpotifySettingsMutation,
		getSpotifySettingsQuery
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import { getSpotifyRedirectUriQuery } from '$lib/features/playlists/queries/spotify.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { Copy, ExternalLink } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

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

	const redirectQuery = getSpotifyRedirectUriQuery(() => section.saved?.spotify_redirect_origin);
	// When the server cannot say, the callback on this page's own origin is the best guess.
	const redirectUri = $derived(
		redirectQuery.data?.redirect_uri ??
			(redirectQuery.isError
				? new URL(
						getApiUrl(v3('/api/v3/acquire/spotify/auth/callback')),
						window.location.origin
					).toString()
				: '')
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
</script>

<div class="space-y-5">
	<SettingsPaneHeader title="Spotify" description="App credentials for the whole instance." />
	<Card variant="bordered">
		<div class="mt-2 flex flex-col gap-4">
			<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-muted">
				These are the shared app credentials for one registered Spotify application. Each user links
				<span class="font-medium">their own</span> Spotify account from their
				<a href={withBasePath('/profile')} class="link link-primary">profile</a> to import their personal
				playlists.
			</div>

			<div class="rounded-xl border border-warning/20 bg-warning/5 p-3 text-sm text-muted">
				<p class="font-medium text-warning/80">Development mode restriction</p>
				<p class="mt-1">
					Spotify apps in development mode only allow up to 5 explicitly allowlisted users. Each
					user who wants to import playlists must be added by email in your
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
							error={section.fieldError('client_id', 'Client ID')}
						/>

						<SecretField
							label="Client Secret"
							mask={SECRET_MASK}
							placeholder="Spotify Client Secret"
							bind:value={draft.client_secret}
							error={section.fieldError('client_secret', 'Client Secret')}
						/>

						<TextField
							label="Redirect Origin (optional)"
							placeholder="https://music.example.com"
							help="Leave this empty and DroppedNeedle uses the address it works out for itself. That is right when you reach it directly, or through a reverse proxy that is listed in TRUSTED_PROXY_IPS and sends X-Forwarded-Proto. Fill it in (no path) when the redirect URI below does not start with the address in your browser's address bar."
							bind:value={draft.spotify_redirect_origin}
							error={section.fieldError('spotify_redirect_origin', 'Redirect Origin (optional)')}
						/>

						<ToggleRow label="Enable Spotify integration" bind:checked={draft.enabled} />

						<Card variant="bordered" padding="sm">
							<div>
								<p class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-muted">
									Redirect URI: add this in your Spotify app dashboard
								</p>
								<div class="flex items-center gap-2">
									<code
										class="min-w-0 flex-1 break-all rounded-lg bg-base-100/60 px-2.5 py-1.5 text-xs text-base-content/80"
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
							class="flex w-fit items-center gap-1 text-xs text-muted transition-colors hover:text-primary"
						>
							<ExternalLink class="h-3 w-3" /> Create a Spotify app to get Client ID + Secret
						</a>

						<SaveBar {...section.bar} saveLabel="Save credentials" />
					</div>
				{/snippet}
			</QueryBoundary>
		</div>
	</Card>
</div>
