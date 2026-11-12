<script lang="ts">
	import '$lib/features/player/components/now-playing.css';
	import { Headphones, Disc3 } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { nowPlayingMerged } from '$lib/features/player/state/nowPlayingMerged.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import YouTubeIcon from '$lib/media/icons/YouTubeIcon.svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import SidebarServiceHint from '$lib/shell/SidebarServiceHint.svelte';
	import SidebarVisualiser from '$lib/shell/SidebarVisualiser.svelte';
	import { servicePath, within, type ServiceSource } from '$lib/shell/navItems';
	import { useServiceEntries } from '$lib/shell/serviceEntries.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		currentPath: string;
	}

	let { currentPath }: Props = $props();

	const services = useServiceEntries();

	/** The rail marks the service whose page is showing, so Library stays the way back. */
	function onService(source: ServiceSource): boolean {
		return within(servicePath(source))(currentPath);
	}

	const localPlaying = $derived(
		playerStore.isPlaying && playerStore.nowPlaying?.sourceType === 'local'
	);

	// An entry, or an admin's hint to set the service up.
	function showsEntry(key: ServiceSource): boolean {
		if (services.hasEntry(key)) return true;
		return !services.isHidden(key) && services.integrations.loaded && authStore.isAdmin;
	}

	const anyEntryVisible = $derived(
		(['youtube', 'jellyfin', 'navidrome', 'plex', 'localfiles'] as const).some(showsEntry)
	);
</script>

{#if services.integrations.loaded && anyEntryVisible}
	<div class="dn-rail__rule" aria-hidden="true"></div>
{/if}

{#if !services.isHidden('youtube')}
	{#if services.hasEntry('youtube')}
		<li>
			<a
				href={withBasePath(servicePath('youtube'))}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={onService('youtube')}
				data-tip="YouTube"
				aria-label="YouTube"
				aria-current={onService('youtube') ? 'page' : undefined}
			>
				<YouTubeIcon class="h-5 w-5 text-error" />
				<span class="is-drawer-close:hidden">YouTube</span>
			</a>
		</li>
	{:else if services.integrations.loaded && authStore.isAdmin}
		<SidebarServiceHint label="YouTube" settingsTab="youtube">
			{#snippet icon()}<YouTubeIcon class="h-5 w-5 text-error" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !services.isHidden('jellyfin')}
	{#if services.hasEntry('jellyfin')}
		<li>
			<a
				href={withBasePath(servicePath('jellyfin'))}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={onService('jellyfin')}
				data-tip="Jellyfin"
				aria-label="Jellyfin"
				aria-current={onService('jellyfin') ? 'page' : undefined}
			>
				<div class="relative inline-flex">
					<JellyfinIcon class="h-5 w-5 text-info" />
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
	{:else if services.integrations.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Jellyfin" settingsTab="jellyfin">
			{#snippet icon()}<JellyfinIcon class="h-5 w-5 text-info" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !services.isHidden('navidrome')}
	{#if services.hasEntry('navidrome')}
		<li>
			<a
				href={withBasePath(servicePath('navidrome'))}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={onService('navidrome')}
				data-tip="Navidrome"
				aria-label="Navidrome"
				aria-current={onService('navidrome') ? 'page' : undefined}
			>
				<div class="relative inline-flex">
					<NavidromeIcon class="h-5 w-5 text-primary" />
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
	{:else if services.integrations.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Navidrome" settingsTab="navidrome">
			{#snippet icon()}<NavidromeIcon class="h-5 w-5 text-primary" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !services.isHidden('plex')}
	{#if services.hasEntry('plex')}
		<li>
			<a
				href={withBasePath(servicePath('plex'))}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={onService('plex')}
				data-tip="Plex"
				aria-label="Plex"
				aria-current={onService('plex') ? 'page' : undefined}
			>
				<div class="relative inline-flex">
					<PlexIcon class="h-5 w-5 text-[rgb(var(--brand-plex))]" />
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
	{:else if services.integrations.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Plex" settingsTab="plex">
			{#snippet icon()}<PlexIcon class="h-5 w-5 text-[rgb(var(--brand-plex))]" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if !services.isHidden('localfiles')}
	{#if services.hasEntry('localfiles')}
		<li>
			<a
				href={withBasePath(servicePath('localfiles'))}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={onService('localfiles')}
				data-tip="Local Files"
				aria-label="Local Files"
				aria-current={onService('localfiles') ? 'page' : undefined}
			>
				<div class="relative inline-flex">
					<Headphones class="h-5 w-5 text-accent" />
					{#if localPlaying}
						<span class="absolute -top-1 -right-1" aria-label="Playing local files">
							<Disc3 class="vinyl-spin h-3 w-3 text-accent" />
						</span>
					{/if}
				</div>
				<span class="is-drawer-close:hidden">Local Files</span>
			</a>
		</li>
	{:else if services.integrations.loaded && authStore.isAdmin}
		<SidebarServiceHint label="Local Files" settingsTab="library">
			{#snippet icon()}<Headphones class="h-5 w-5 text-accent" />{/snippet}
		</SidebarServiceHint>
	{/if}
{/if}

{#if authStore.isAdmin}
	<SidebarVisualiser />
{/if}

<style>
	/* an active service's mark takes the fill's ink, whatever its brand colour */
	a:global(.menu-active) :global(svg) {
		color: inherit;
	}
</style>
