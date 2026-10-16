<script lang="ts">
	import { Database, ExternalLink } from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import type { ProfileServiceConnection } from '$lib/queries/profile/types';
	import Card from '$lib/ui/Card.svelte';

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

<section id="connected-services" class="scroll-mt-24 xl:ml-40">
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<ExternalLink class="h-4 w-4" />
		Connected Services
	</h2>
	<div class="grid gap-3 sm:grid-cols-3">
		{#each services as service (service.name)}
			{@const Icon = getServiceIcon(service.name)}
			<Card variant="bordered" padding="md">
				<div class="flex items-center gap-3">
					<div
						class="flex h-10 w-10 items-center justify-center rounded-lg bg-base-300/60 {getServiceColor(
							service.name
						)}"
					>
						<Icon class="h-5 w-5" />
					</div>
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-2">
							<span class="text-sm font-semibold">{service.name}</span>
							{#if service.enabled}
								<span class="status status-success status-sm"></span>
							{:else}
								<span class="status status-error status-sm"></span>
							{/if}
						</div>
						{#if service.enabled && service.username}
							<p class="mt-0.5 truncate text-xs text-base-content/50">
								{service.username}
							</p>
						{:else if !service.enabled}
							<p class="mt-0.5 text-xs text-base-content/30">Not connected</p>
						{/if}
					</div>
				</div>
			</Card>
		{/each}
	</div>
</section>
