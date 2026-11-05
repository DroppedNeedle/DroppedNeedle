<script lang="ts">
	import { Database, ExternalLink } from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import type { ProfileServiceConnection } from '$lib/features/profile/queries/profile.svelte';

	interface Props {
		services: ProfileServiceConnection[];
	}

	let { services }: Props = $props();

	function getServiceIcon(name: string) {
		if (name === 'Jellyfin') return JellyfinIcon;
		if (name === 'Navidrome') return NavidromeIcon;
		if (name === 'Plex') return PlexIcon;
		return Database;
	}

	function getServiceColor(name: string): string {
		if (name === 'Jellyfin') return 'text-jellyfin';
		if (name === 'Navidrome') return 'text-navidrome';
		if (name === 'Plex') return 'text-plex';
		return 'text-base-content';
	}
</script>

<ul class="grid gap-3 sm:grid-cols-3">
	{#each services as service (service.name)}
		{@const Icon = getServiceIcon(service.name)}
		{@const href = outsideLink(service.url)}
		<li class="pf-box cs-tile" class:cs-tile--link={href}>
			{#if href}
				<a
					{href}
					target="_blank"
					rel="noopener noreferrer"
					aria-label="Open {service.name}"
					class="cs-tile__link"
				></a>
			{/if}
			<span class="cs-tile__icon {getServiceColor(service.name)}" aria-hidden="true">
				<Icon class="size-6" />
			</span>
			<div class="min-w-0 flex-1">
				<p class="flex items-center gap-2 font-semibold">
					{service.name}
					<span
						class="status status-sm {service.enabled ? 'status-success' : 'status-error'}"
						aria-hidden="true"
					></span>
				</p>
				{#if service.enabled && service.username}
					<p class="pf-meta truncate">{service.username}</p>
				{:else if !service.enabled}
					<p class="pf-meta">Not connected</p>
				{/if}
			</div>
			{#if href}
				<ExternalLink class="size-4 shrink-0 text-muted" aria-hidden="true" />
			{/if}
		</li>
	{/each}
</ul>

<style>
	.cs-tile {
		position: relative;
		display: flex;
		align-items: center;
		gap: 0.875rem;
		padding: 0.875rem 1rem;
		transition: border-color 150ms;
	}
	.cs-tile--link:hover {
		border-color: var(--color-base-content);
	}
	.cs-tile__link {
		position: absolute;
		inset: 0;
		border-radius: inherit;
	}
	.cs-tile__link:focus-visible {
		outline: 3px solid var(--dn-focus);
		outline-offset: 2px;
	}
	.cs-tile__icon {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		border-radius: 0.5rem;
		background-color: var(--color-base-300);
	}
</style>
