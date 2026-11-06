<script lang="ts">
	import {
		CircleAlert,
		FolderOpen,
		KeyRound,
		Library,
		ListMusic,
		Link2,
		LogOut,
		RadioTower,
		RefreshCw,
		ServerCog,
		Settings,
		SunMoon,
		UserCog,
		Waypoints
	} from 'lucide-svelte';
	import { tick } from 'svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { logout } from '$lib/utils/logout';
	import { getProfileQuery } from '$lib/features/profile/queries/profile.svelte';
	import {
		getRemoteConnectionQuery,
		getRemoteFoldersQuery,
		getRemoteStatsQuery
	} from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import type {
		ProfileLibraryStats,
		ProfileServiceConnection
	} from '$lib/features/profile/queries/profile.svelte';
	import { getSessionsQuery } from '$lib/features/profile/queries/sessions.svelte';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import {
		APP_PASSWORD_CAP,
		getAppPasswordsQuery
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import AccountCard from '$lib/features/profile/components/AccountCard.svelte';
	import ConnectedServicesGrid from '$lib/features/profile/components/ConnectedServicesGrid.svelte';
	import LibraryStatsList from '$lib/features/profile/components/LibraryStatsList.svelte';
	import MediaServerAccountsCard from '$lib/features/profile/components/MediaServerAccountsCard.svelte';
	import NavidromeMusicFoldersCard from '$lib/features/profile/components/NavidromeMusicFoldersCard.svelte';
	import ProfileHeader from '$lib/features/profile/components/ProfileHeader.svelte';
	import ProfileSection from '$lib/features/profile/components/ProfileSection.svelte';
	import SessionsManager from '$lib/features/profile/components/SessionsManager.svelte';
	import AppearanceCard from '$lib/features/profile/components/AppearanceCard.svelte';
	import ScrobblingDiscoveryCard from '$lib/features/profile/components/ScrobblingDiscoveryCard.svelte';
	import SpotifyConnectionCard from '$lib/features/profile/components/SpotifyConnectionCard.svelte';
	import ProfileConnectApps from '$lib/features/profile/components/ProfileConnectApps.svelte';
	import '$lib/features/profile/components/profile.css';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import JumpTabs, { type JumpSection } from '$lib/ui/JumpTabs.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { page } from '$app/state';
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import { toastStore } from '$lib/stores/toast';
	import { refreshConnections } from '$lib/features/profile/queries/ConnectionsMutations.svelte';

	const userId = authStore.user?.id ?? '';
	const profileQuery = getProfileQuery(() => authStore.user?.id ?? '');
	const navidromeConnection = getRemoteConnectionQuery(() => 'navidrome');
	const jellyfinConnection = getRemoteConnectionQuery(() => 'jellyfin');
	const plexConnection = getRemoteConnectionQuery(() => 'plex');
	const navidromeStats = getRemoteStatsQuery(() => 'navidrome');
	const jellyfinStats = getRemoteStatsQuery(() => 'jellyfin');
	const plexStats = getRemoteStatsQuery(() => 'plex');
	const libraryStatsQuery = getLibraryStatsQuery();
	// the closed sections' summaries read the same queries their sections do
	const sessionsQuery = getSessionsQuery();
	const connectionsQuery = getConnectionsQuery();
	const appPasswordsQuery = getAppPasswordsQuery();
	const profile = $derived(profileQuery.data);
	const providers = $derived(profile?.providers ?? authStore.user?.providers ?? []);
	const hasLocalPassword = $derived(providers.includes('local'));

	// v3 split the old fat profile: services come from the remotes
	// connection reads (a server counts as usable in either account mode),
	// libraries from the per-source stats reads plus the global stats for
	// local files. Both stay empty until their reads settle so the grid
	// never flashes red-then-green on a cold nav.
	const services = $derived.by<ProfileServiceConnection[]>(() => {
		if (navidromeConnection.isPending || jellyfinConnection.isPending || plexConnection.isPending)
			return [];
		return [
			{
				name: 'Navidrome',
				enabled: navidromeConnection.data?.connected ?? false,
				username: navidromeConnection.data?.account_label ?? '',
				url: navidromeConnection.data?.server_url ?? null
			},
			{
				name: 'Jellyfin',
				enabled: jellyfinConnection.data?.connected ?? false,
				username: jellyfinConnection.data?.account_label ?? '',
				url: jellyfinConnection.data?.server_url ?? null
			},
			{
				name: 'Plex',
				enabled: plexConnection.data?.connected ?? false,
				username: plexConnection.data?.account_label ?? '',
				url: plexConnection.data?.server_url ?? null
			}
		];
	});
	const libraryStats = $derived.by<ProfileLibraryStats[]>(() => {
		const rows: ProfileLibraryStats[] = [];
		const remotes = [
			{ source: 'Navidrome', status: navidromeConnection, stats: navidromeStats },
			{ source: 'Jellyfin', status: jellyfinConnection, stats: jellyfinStats },
			{ source: 'Plex', status: plexConnection, stats: plexStats }
		];
		for (const { source, status, stats } of remotes) {
			if (!status.data?.connected || stats.isPending || !stats.data) continue;
			rows.push({
				source,
				total_tracks: stats.data.total_tracks,
				total_albums: stats.data.total_albums,
				total_artists: stats.data.total_artists,
				total_size_bytes: null,
				total_size_human: null
			});
		}
		const global = libraryStatsQuery.data;
		if (global && global.total_tracks > 0) {
			rows.push({
				source: 'Local Files',
				total_tracks: global.total_tracks,
				total_albums: global.total_albums,
				total_artists: global.total_artists,
				total_size_bytes: global.total_size_bytes,
				total_size_human: formatBytes(global.total_size_bytes)
			});
		}
		return rows;
	});

	const navidromeEnabled = $derived(
		services.some((service) => service.name === 'Navidrome' && service.enabled)
	);
	const mediaAccountsEnabled = $derived(
		services.some(
			(service) => service.enabled && ['Navidrome', 'Jellyfin', 'Plex'].includes(service.name)
		)
	);
	const foldersQuery = getRemoteFoldersQuery(() => Boolean(userId) && navidromeEnabled);

	const sections = $derived.by<JumpSection[]>(() => {
		if (!profile) return [];
		return [
			{ id: 'account', label: 'Account', icon: UserCog },
			{ id: 'connect-apps', label: 'Connect Apps', icon: Waypoints },
			{ id: 'appearance', label: 'Appearance', icon: SunMoon },
			{ id: 'sessions', label: 'Sessions', icon: KeyRound },
			{ id: 'connected-services', label: 'Connected Services', icon: Link2 },
			...(mediaAccountsEnabled
				? [{ id: 'media-accounts', label: 'Media Server Accounts', icon: ServerCog }]
				: []),
			...(navidromeEnabled
				? [{ id: 'navidrome-music-folders', label: 'Music Folders', icon: FolderOpen }]
				: []),
			{ id: 'scrobbling', label: 'Scrobbling', icon: RadioTower },
			{ id: 'spotify', label: 'Spotify', icon: ListMusic },
			...(libraryStats.length > 0
				? [{ id: 'libraries', label: 'Your Libraries', icon: Library }]
				: [])
		];
	});

	// Which sections are open lasts the browser session. Account starts open;
	// a jump or a link to a section opens it.
	const OPEN_KEY = 'droppedneedle_profile_open';
	function readOpen(): string[] {
		if (!browser) return ['account'];
		try {
			const saved = JSON.parse(sessionStorage.getItem(OPEN_KEY) ?? 'null');
			if (Array.isArray(saved)) return saved.filter((id) => typeof id === 'string');
		} catch {
			// a value that does not parse starts over
		}
		return ['account'];
	}
	let openIds = $state<string[]>(readOpen());
	function setOpen(id: string, open: boolean) {
		const next = openIds.filter((other) => other !== id);
		if (open) next.push(id);
		openIds = next;
		try {
			sessionStorage.setItem(OPEN_KEY, JSON.stringify(next));
		} catch {
			// storage refused (private mode): the choice lasts this page only
		}
	}
	const isOpen = (id: string) => openIds.includes(id);

	// Open and scroll to a deep-link anchor (e.g. /profile#connect-apps) once
	// the async profile content has rendered. SvelteKit's built-in scroll
	// fires before the {#if profile} body exists on a cold nav.
	let scrolledToHash: string | null = null;
	$effect(() => {
		if (!browser || !profile) return;
		const id = page.url.hash.slice(1);
		if (!id || scrolledToHash === id) return;
		scrolledToHash = id;
		if (!isOpen(id)) setOpen(id, true);
		const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		void tick().then(() =>
			requestAnimationFrame(() =>
				document
					.getElementById(id)
					?.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' })
			)
		);
	});

	onMount(async () => {
		const spotify = page.url.searchParams.get('spotify');
		if (spotify === 'connected') {
			toastStore.show({ message: 'Spotify connected successfully', type: 'success' });
			await refreshConnections();
			if (browser) history.replaceState({}, '', withBasePath('/profile'));
		} else if (spotify === 'error') {
			toastStore.show({ message: 'Spotify connection failed. Please try again.', type: 'error' });
			if (browser) history.replaceState({}, '', withBasePath('/profile'));
		}
	});

	// summaries for the closed rows: only what the page already reads
	const connections = $derived(connectionsQuery.data?.connections ?? []);
	const linkedTo = (service: string) => connections.find((c) => c.service === service);
	const sessionCount = $derived(sessionsQuery.data?.sessions.length ?? null);
	const appPasswordCount = $derived(appPasswordsQuery.data?.length ?? null);
	const mediaRows = $derived(
		services.filter((service) => service.enabled).map((service) => service.name.toLowerCase())
	);
	const mediaLinked = $derived(mediaRows.filter((service) => linkedTo(service)).length);
	const themeLabel = {
		dark: 'Dark',
		light: 'Light',
		midnight: 'Midnight',
		system: 'System'
	} as const;
	const count = (n: number) => n.toLocaleString();
	const plural = (n: number, one: string, many: string) => `${count(n)} ${n === 1 ? one : many}`;
</script>

<svelte:head>
	<title>Profile - DroppedNeedle</title>
</svelte:head>

{#snippet dot(name: string, ok: boolean)}
	<span class="pf-dot">
		<span class="status status-sm {ok ? 'status-success' : 'status-error'}" aria-hidden="true"
		></span>
		{name}<span class="sr-only">: {ok ? 'connected' : 'not connected'}</span>
	</span>
{/snippet}

{#snippet spotifyGlyph(size: string)}
	<SpotifyIcon class={size} />
{/snippet}

<div class="pf-page">
	{#if profileQuery.isPending}
		<div class="flex items-center gap-7 px-8 pt-6 pb-7">
			<div class="skeleton size-36 shrink-0 rounded-full"></div>
			<div class="flex flex-col gap-3">
				<div class="skeleton h-4 w-40"></div>
				<div class="skeleton h-20 w-96 max-w-[60vw]"></div>
			</div>
		</div>
	{:else if profile}
		<ProfileHeader {profile} />
	{:else if profileQuery.isError}
		<div class="flex flex-col items-center gap-4 py-16 text-center">
			<CircleAlert class="size-10 text-muted" />
			<p class="text-muted">Failed to load profile</p>
			<Button
				variant="primary"
				size="display-sm"
				icon={RefreshCw}
				onclick={() => void profileQuery.refetch()}
			>
				Try Again
			</Button>
		</div>
	{/if}

	{#if profile}
		<JumpTabs
			{sections}
			look="pills"
			label="On this page"
			onjump={(id) => setOpen(id, true)}
			class="xl:hidden"
		/>

		<div class="pf-layout">
			<aside class="pf-rail">
				<JumpTabs {sections} look="rail" label="On this page" onjump={(id) => setOpen(id, true)} />
			</aside>

			<div class="pf-sections stagger-fade-in">
				<ProfileSection
					id="account"
					title="Account"
					blurb="Your sign-in details and how you access DroppedNeedle."
					icon={UserCog}
					open={isOpen('account')}
					ontoggle={(open) => setOpen('account', open)}
				>
					{#snippet summary()}
						<span class="normal-case tracking-normal">
							@{profile.username_display ?? profile.username ?? '-'}
						</span>
						{#if profile.email}<span class="normal-case tracking-normal">{profile.email}</span>{/if}
					{/snippet}
					<AccountCard {profile} {hasLocalPassword} />
				</ProfileSection>

				<ProfileSection
					id="connect-apps"
					title="Connect Apps"
					blurb="App-passwords and how to connect outside apps (OpenSubsonic, Jellyfin)."
					icon={Waypoints}
					open={isOpen('connect-apps')}
					ontoggle={(open) => setOpen('connect-apps', open)}
				>
					{#snippet summary()}
						{#if appPasswordCount !== null}
							<span>{appPasswordCount} / {APP_PASSWORD_CAP} app-passwords</span>
						{/if}
					{/snippet}
					<ProfileConnectApps />
				</ProfileSection>

				<ProfileSection
					id="appearance"
					title="Appearance"
					blurb="How DroppedNeedle looks for you in this browser."
					icon={SunMoon}
					open={isOpen('appearance')}
					ontoggle={(open) => setOpen('appearance', open)}
				>
					{#snippet summary()}
						<span>Theme · {themeLabel[themeStore.preference]}</span>
					{/snippet}
					<AppearanceCard />
				</ProfileSection>

				<ProfileSection
					id="sessions"
					title="Sessions"
					blurb="Your signed-in sessions across browsers and apps."
					icon={KeyRound}
					open={isOpen('sessions')}
					ontoggle={(open) => setOpen('sessions', open)}
				>
					{#snippet summary()}
						{#if sessionCount !== null}
							<span>{plural(sessionCount, 'signed-in session', 'signed-in sessions')}</span>
						{/if}
					{/snippet}
					<SessionsManager />
				</ProfileSection>

				<ProfileSection
					id="connected-services"
					title="Connected Services"
					blurb="The media servers this DroppedNeedle reads from."
					icon={Link2}
					open={isOpen('connected-services')}
					ontoggle={(open) => setOpen('connected-services', open)}
				>
					{#snippet summary()}
						{#if services.length > 0}
							<span>{plural(services.length, 'service', 'services')}</span>
							{#each services as service (service.name)}
								{@render dot(service.name, service.enabled)}
							{/each}
						{/if}
					{/snippet}
					<ConnectedServicesGrid {services} />
				</ProfileSection>

				{#if mediaAccountsEnabled}
					<ProfileSection
						id="media-accounts"
						title="Media Server Accounts"
						blurb="Your own account on each server, so plays count as you."
						icon={ServerCog}
						open={isOpen('media-accounts')}
						ontoggle={(open) => setOpen('media-accounts', open)}
					>
						{#snippet summary()}
							{#if connectionsQuery.data}
								<span>{mediaLinked} of {mediaRows.length} linked</span>
							{/if}
						{/snippet}
						<MediaServerAccountsCard {services} />
					</ProfileSection>
				{/if}

				{#if navidromeEnabled}
					<ProfileSection
						id="navidrome-music-folders"
						title="Music Folders"
						blurb="Which Navidrome folders appear in your catalog."
						icon={FolderOpen}
						open={isOpen('navidrome-music-folders')}
						ontoggle={(open) => setOpen('navidrome-music-folders', open)}
					>
						{#snippet summary()}
							{#if foldersQuery.data}
								<span>
									{foldersQuery.data.mode === 'selected'
										? `Selected folders · ${foldersQuery.data.folder_ids.length}`
										: 'All folders'}
								</span>
							{/if}
						{/snippet}
						<NavidromeMusicFoldersCard {userId} />
					</ProfileSection>
				{/if}

				<ProfileSection
					id="scrobbling"
					title="Scrobbling & Discovery"
					blurb="Where your plays go and what powers Home and Discover."
					icon={RadioTower}
					open={isOpen('scrobbling')}
					ontoggle={(open) => setOpen('scrobbling', open)}
				>
					{#snippet summary()}
						{#if connectionsQuery.data}
							{@render dot('ListenBrainz', Boolean(linkedTo('listenbrainz')))}
							{@render dot('Last.fm', Boolean(linkedTo('lastfm')?.username))}
						{/if}
					{/snippet}
					<ScrobblingDiscoveryCard {navidromeEnabled} />
				</ProfileSection>

				<ProfileSection
					id="spotify"
					title="Spotify"
					blurb="Import your own Spotify playlists."
					glyph={spotifyGlyph}
					open={isOpen('spotify')}
					ontoggle={(open) => setOpen('spotify', open)}
				>
					{#snippet summary()}
						{#if connectionsQuery.data}
							{@const spotify = linkedTo('spotify')}
							{@render dot(
								spotify ? `@${spotify.username || 'linked'}` : 'Spotify',
								Boolean(spotify)
							)}
						{/if}
					{/snippet}
					<SpotifyConnectionCard />
				</ProfileSection>

				{#if libraryStats.length > 0}
					<ProfileSection
						id="libraries"
						title="Your Libraries"
						blurb="Song, album and artist counts for each source."
						icon={Library}
						open={isOpen('libraries')}
						ontoggle={(open) => setOpen('libraries', open)}
					>
						{#snippet summary()}
							<span>{plural(libraryStats.length, 'library', 'libraries')}</span>
							{#each libraryStats as stats (stats.source)}
								<span>{stats.source} · {plural(stats.total_tracks, 'track', 'tracks')}</span>
							{/each}
						{/snippet}
						<LibraryStatsList {libraryStats} />
					</ProfileSection>
				{/if}

				<div class="pf-foot">
					<Button
						variant="outline"
						size="display-sm"
						icon={Settings}
						href={withBasePath('/settings')}
					>
						Open Settings
					</Button>
					<Button
						variant="danger-outline"
						size="display-sm"
						icon={LogOut}
						onclick={() => void logout()}
					>
						Sign Out
					</Button>
				</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.pf-page {
		min-height: 100vh;
		padding-bottom: 3rem;
	}

	.pf-layout {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 2rem;
		max-width: 92rem;
		padding: 1.25rem 1rem 0;
	}
	@media (min-width: 640px) {
		.pf-layout {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.pf-layout {
			padding-inline: 2rem;
		}
	}
	.pf-rail {
		display: none;
	}
	@media (min-width: 1280px) {
		.pf-layout {
			grid-template-columns: 14.5rem minmax(0, 1fr);
			padding-top: 0.5rem;
		}
		.pf-rail {
			display: block;
		}
	}

	.pf-sections {
		display: flex;
		flex-direction: column;
		gap: 0.875rem;
		min-width: 0;
	}
	/* under xl the jump strip sits under the top bar too */
	@media (max-width: 1279px) {
		.pf-sections :global(.pf-sec) {
			scroll-margin-top: 8.5rem;
		}
	}

	.pf-dot {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.25rem 0.75rem;
		border: 1px solid var(--dn-edge);
		border-radius: 9999px;
		font-family: var(--font-sans);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0;
		text-transform: none;
	}

	.pf-foot {
		display: flex;
		flex-wrap: wrap;
		gap: 0.875rem;
		padding-top: 0.75rem;
	}
	@media (max-width: 639px) {
		.pf-foot {
			justify-content: center;
		}
	}
</style>
