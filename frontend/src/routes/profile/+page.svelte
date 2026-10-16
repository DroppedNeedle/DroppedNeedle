<script lang="ts">
	import { CircleAlert, LogOut, RefreshCw, Settings } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { logout } from '$lib/utils/logout';
	import { getProfileQuery } from '$lib/queries/profile/ProfileQuery.svelte';
	import {
		getRemoteConnectionQuery,
		getRemoteStatsQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import { getLibraryStatsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import type { ProfileLibraryStats, ProfileServiceConnection } from '$lib/queries/profile/types';
	import AccountCard from '$lib/components/profile/AccountCard.svelte';
	import ConnectedServicesGrid from '$lib/components/profile/ConnectedServicesGrid.svelte';
	import LibraryStatsList from '$lib/components/profile/LibraryStatsList.svelte';
	import MediaServerAccountsCard from '$lib/components/profile/MediaServerAccountsCard.svelte';
	import NavidromeMusicFoldersCard from '$lib/components/profile/NavidromeMusicFoldersCard.svelte';
	import ProfileHeader from '$lib/components/profile/ProfileHeader.svelte';
	import SessionsManager from '$lib/components/profile/SessionsManager.svelte';
	import AppearanceCard from '$lib/components/profile/AppearanceCard.svelte';
	import ScrobblingDiscoveryCard from '$lib/components/profile/ScrobblingDiscoveryCard.svelte';
	import SpotifyConnectionCard from '$lib/components/profile/SpotifyConnectionCard.svelte';
	import ProfileConnectApps from '$lib/components/profile/ProfileConnectApps.svelte';
	import PageSectionToc from '$lib/components/PageSectionToc.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { page } from '$app/state';
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import { toastStore } from '$lib/stores/toast';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { ConnectionsQueryKeyFactory } from '$lib/queries/connections/ConnectionsQueryKeyFactory';

	const userId = authStore.user?.id ?? '';
	const profileQuery = getProfileQuery(() => authStore.user?.id ?? '');
	const navidromeConnection = getRemoteConnectionQuery(() => 'navidrome');
	const jellyfinConnection = getRemoteConnectionQuery(() => 'jellyfin');
	const plexConnection = getRemoteConnectionQuery(() => 'plex');
	const navidromeStats = getRemoteStatsQuery(() => 'navidrome');
	const jellyfinStats = getRemoteStatsQuery(() => 'jellyfin');
	const plexStats = getRemoteStatsQuery(() => 'plex');
	const libraryStatsQuery = getLibraryStatsQuery();
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
				url: null
			},
			{
				name: 'Jellyfin',
				enabled: jellyfinConnection.data?.connected ?? false,
				username: jellyfinConnection.data?.account_label ?? '',
				url: null
			},
			{
				name: 'Plex',
				enabled: plexConnection.data?.connected ?? false,
				username: plexConnection.data?.account_label ?? '',
				url: null
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

	type ProfileTocSection = {
		id: string;
		label: string;
	};

	const profileTocSections = $derived.by<ProfileTocSection[]>(() => {
		if (!profile) return [];

		return [
			{ id: 'account', label: 'Account' },
			{ id: 'appearance', label: 'Appearance' },
			{ id: 'sessions', label: 'Sessions' },
			{ id: 'connected-services', label: 'Connected Services' },
			...(mediaAccountsEnabled ? [{ id: 'media-accounts', label: 'Media Accounts' }] : []),
			...(navidromeEnabled ? [{ id: 'navidrome-music-folders', label: 'Music Folders' }] : []),
			{ id: 'connect-apps', label: 'Connect Apps' },
			{ id: 'scrobbling', label: 'Scrobbling' },
			{ id: 'spotify', label: 'Spotify' },
			...(libraryStats.length > 0 ? [{ id: 'libraries', label: 'Your Libraries' }] : [])
		];
	});

	// Scroll to a deep-link anchor (e.g. /profile#connect-apps) once the async
	// profile content has rendered. SvelteKit's built-in scroll fires before the
	// {#if profile} body exists on a cold nav, so nothing scrolls without this.
	let scrolledToHash: string | null = null;
	$effect(() => {
		if (!browser || !profile) return;
		const id = page.url.hash.slice(1);
		if (!id || scrolledToHash === id) return;
		scrolledToHash = id;
		const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		requestAnimationFrame(() =>
			document
				.getElementById(id)
				?.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' })
		);
	});

	onMount(async () => {
		const spotify = page.url.searchParams.get('spotify');
		if (spotify === 'connected') {
			toastStore.show({ message: 'Spotify connected successfully', type: 'success' });
			await invalidateQueriesWithPersister({
				queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id)
			});
			if (browser) history.replaceState({}, '', withBasePath('/profile'));
		} else if (spotify === 'error') {
			toastStore.show({ message: 'Spotify connection failed. Please try again.', type: 'error' });
			if (browser) history.replaceState({}, '', withBasePath('/profile'));
		}
	});
</script>

<svelte:head>
	<title>Profile - DroppedNeedle</title>
</svelte:head>

<div class="min-h-screen">
	<div class="relative overflow-hidden">
		<div class="absolute inset-0 bg-linear-to-br from-primary/20 via-accent/10 to-base-100"></div>
		<div class="absolute inset-0 bg-linear-to-t from-base-100 via-base-100/60 to-transparent"></div>

		<div class="relative px-4 pt-10 pb-6 sm:px-6 lg:px-8">
			<div class="mx-auto max-w-4xl">
				{#if profileQuery.isPending}
					<div class="flex flex-col items-center gap-6">
						<div class="skeleton h-32 w-32 rounded-full sm:h-40 sm:w-40"></div>
						<div class="flex flex-col items-center gap-2">
							<div class="skeleton h-8 w-48"></div>
							<div class="skeleton h-4 w-32"></div>
						</div>
					</div>
				{:else if profile}
					<ProfileHeader {profile} />
				{:else if profileQuery.isError}
					<div class="flex flex-col items-center gap-4 py-12 text-center">
						<CircleAlert class="h-10 w-10 text-base-content/50" />
						<p class="text-base-content/70">Failed to load profile</p>
						<Button
							variant="primary"
							size="sm"
							icon={RefreshCw}
							onclick={() => void profileQuery.refetch()}
						>
							Try Again
						</Button>
					</div>
				{/if}
			</div>
		</div>
	</div>

	{#if profile}
		<div class="px-4 pb-12 sm:px-6 lg:px-8">
			<div class="relative mx-auto flex max-w-4xl flex-col gap-8 stagger-fade-in xl:max-w-[66rem]">
				<PageSectionToc
					sections={profileTocSections}
					className="xl:absolute xl:inset-y-0 xl:left-0 xl:w-36"
				/>

				<AccountCard {profile} {hasLocalPassword} />

				<div id="appearance" class="scroll-mt-24 xl:ml-40">
					<AppearanceCard />
				</div>

				<div id="sessions" class="scroll-mt-24 xl:ml-40">
					<SessionsManager />
				</div>

				<ConnectedServicesGrid {services} />

				<div id="media-accounts" class="scroll-mt-24 xl:ml-40">
					<MediaServerAccountsCard {services} />
				</div>

				<div id="navidrome-music-folders" class="scroll-mt-24 xl:ml-40">
					<NavidromeMusicFoldersCard {userId} />
				</div>

				<div id="connect-apps" class="scroll-mt-24 xl:ml-40">
					<ProfileConnectApps />
				</div>

				<div id="scrobbling" class="scroll-mt-24 xl:ml-40">
					<ScrobblingDiscoveryCard {navidromeEnabled} />
				</div>

				<div id="spotify" class="scroll-mt-24 xl:ml-40">
					<SpotifyConnectionCard />
				</div>

				{#if libraryStats.length > 0}
					<LibraryStatsList {libraryStats} />
				{/if}

				<section class="flex justify-center gap-3 pt-2 xl:ml-40">
					<Button variant="outline" size="sm" pill icon={Settings} href={withBasePath('/settings')}>
						Open Settings
					</Button>
					<Button variant="outline" size="sm" pill icon={LogOut} onclick={() => void logout()}>
						Sign Out
					</Button>
				</section>
			</div>
		</div>
	{/if}
</div>
