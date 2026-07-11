<script lang="ts">
	import { resolve } from '$app/paths';
	import { fromStore } from 'svelte/store';
	import { Headphones, Disc3 } from 'lucide-svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { getSectionPrefsQuery } from '$lib/queries/section-prefs/SectionPrefsQuery.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SidebarServiceHint from '$lib/components/SidebarServiceHint.svelte';
	import SidebarVisualiser from '$lib/components/SidebarVisualiser.svelte';

	const integrations = fromStore(integrationStore);

	const prefsQuery = getSectionPrefsQuery();
	// fail-open: until prefs load, every service is visible
	const hidden = $derived(
		new Set((prefsQuery.data?.pages?.sidebar ?? []).filter((s) => !s.enabled).map((s) => s.key))
	);

	const localPlaying = $derived(
		playerStore.isPlaying && playerStore.nowPlaying?.sourceType === 'local'
	);

	function showsEntry(key: string, connected: boolean): boolean {
		if (hidden.has(key)) return false;
		return connected || (integrations.current.loaded && authStore.isAdmin);
	}

	const anyEntryVisible = $derived(
		showsEntry('youtube', integrations.current.youtube) ||
			showsEntry('jellyfin', integrations.current.jellyfin) ||
			showsEntry('navidrome', integrations.current.navidrome) ||
			showsEntry('plex', integrations.current.plex) ||
			showsEntry('localfiles', integrations.current.localfiles)
	);
</script>

{#if integrations.current.loaded && anyEntryVisible}
	<div class="divider my-0"></div>
{/if}

{#if !hidden.has('youtube')}
	{#if integrations.current.youtube}
		<li>
			<a
				href={resolve('/library/youtube')}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip="YouTube"
			>
				<YouTubeIcon class="h-6 w-6 text-error" />
				<span class="is-drawer-close:hidden">YouTube</span>
			</a>
		</li>
	{:else if integrations.current.loaded && authStore.isAdmin}
		<SidebarServiceHint label="YouTube" settingsTab="youtube">
			{#snippet icon()}<YouTubeIcon class="h-6 w-6 text-error" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !hidden.has('jellyfin')}
	{#if integrations.current.jellyfin}
		<li>
			<a
				href={resolve('/library/jellyfin')}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip="Jellyfin"
			>
				<div class="relative inline-flex">
					<JellyfinIcon class="h-6 w-6 text-info" />
					{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
						<span class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
						></span>
					{/if}
				</div>
				<span class="is-drawer-close:hidden">Jellyfin</span>
				{#if nowPlayingMerged.isSourcePlaying('jellyfin')}
					<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
						<span></span><span></span><span></span>
					</div>
				{/if}
			</a>
		</li>
	{:else if integrations.current.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Jellyfin" settingsTab="jellyfin">
			{#snippet icon()}<JellyfinIcon class="h-6 w-6 text-info" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !hidden.has('navidrome')}
	{#if integrations.current.navidrome}
		<li>
			<a
				href={resolve('/library/navidrome')}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip="Navidrome"
			>
				<div class="relative inline-flex">
					<NavidromeIcon class="h-6 w-6 text-primary" />
					{#if nowPlayingMerged.isSourcePlaying('navidrome')}
						<span class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
						></span>
					{/if}
				</div>
				<span class="is-drawer-close:hidden">Navidrome</span>
				{#if nowPlayingMerged.isSourcePlaying('navidrome')}
					<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
						<span></span><span></span><span></span>
					</div>
				{/if}
			</a>
		</li>
	{:else if integrations.current.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Navidrome" settingsTab="navidrome">
			{#snippet icon()}<NavidromeIcon class="h-6 w-6 text-primary" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !hidden.has('plex')}
	{#if integrations.current.plex}
		<li>
			<a
				href={resolve('/library/plex')}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip="Plex"
			>
				<div class="relative inline-flex">
					<PlexIcon class="h-6 w-6" style="color: rgb(var(--brand-plex))" />
					{#if nowPlayingMerged.isSourcePlaying('plex')}
						<span class="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-primary animate-pulse"
						></span>
					{/if}
				</div>
				<span class="is-drawer-close:hidden">Plex</span>
				{#if nowPlayingMerged.isSourcePlaying('plex')}
					<div class="now-playing-bars now-playing-bars--sm ml-auto is-drawer-close:hidden">
						<span></span><span></span><span></span>
					</div>
				{/if}
			</a>
		</li>
	{:else if integrations.current.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Plex" settingsTab="plex">
			{#snippet icon()}<PlexIcon class="h-6 w-6" style="color: rgb(var(--brand-plex))" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !hidden.has('localfiles')}
	{#if integrations.current.localfiles}
		<li>
			<a
				href={resolve('/library/local')}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip="Local Files"
			>
				<div class="relative inline-flex">
					<Headphones class="h-6 w-6 text-accent" />
					{#if localPlaying}
						<span class="absolute -top-1 -right-1" aria-label="Playing local files">
							<Disc3 class="vinyl-spin h-3 w-3 text-accent" />
						</span>
					{/if}
				</div>
				<span class="is-drawer-close:hidden">Local Files</span>
			</a>
		</li>
	{:else if integrations.current.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Local Files" settingsTab="library">
			{#snippet icon()}<Headphones class="h-6 w-6 text-accent" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if authStore.isAdmin}
	<SidebarVisualiser />
{/if}
