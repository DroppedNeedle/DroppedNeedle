<script lang="ts" module>
	import type { ServicePrompt } from '$lib/features/discover/queries/types';

	// Media servers and download clients are set up under the admin's
	// Settings tabs, so only an admin can act on their cards.
	const adminOnly = new Set<ServicePrompt['service']>(['jellyfin', 'download-client']);

	/** The cards this person can act on. */
	export function promptsFor(prompts: ServicePrompt[], isAdmin: boolean): ServicePrompt[] {
		return isAdmin ? prompts : prompts.filter((prompt) => !adminOnly.has(prompt.service));
	}
</script>

<script lang="ts">
	import {
		BarChart3,
		Bell,
		Check,
		Download,
		Headphones,
		History,
		Link,
		Music,
		Radio,
		Star,
		Tag,
		Tv,
		Users,
		X
	} from 'lucide-svelte';
	import { dismiss } from '$lib/features/discover/state/dismissedPrompts';
	import { withBasePath } from '$lib/utils/basePath';
	import type { ComponentType } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	const serviceIcons: Record<string, ComponentType> = {
		listenbrainz: Headphones,
		jellyfin: Tv,
		'download-client': Download,
		lastfm: Radio
	};

	// The service's own colour on the card's left edge and its mark, as whole
	// class strings so Tailwind's scan finds them.
	const serviceBrands: Record<string, { edge: string; mark: string }> = {
		listenbrainz: { edge: 'border-l-listenbrainz', mark: 'text-listenbrainz' },
		jellyfin: { edge: 'border-l-jellyfin', mark: 'text-jellyfin' },
		lastfm: { edge: 'border-l-lastfm', mark: 'text-lastfm' }
	};
	const otherBrand = { edge: 'border-l-primary', mark: 'text-title' };

	// A small mark per feature label, matched on its words; a tick otherwise.
	const featureIcons: [RegExp, ComponentType][] = [
		[/recommend/i, Star],
		[/release|alert/i, Bell],
		[/stat/i, BarChart3],
		[/genre/i, Tag],
		[/scrobbl/i, Radio],
		[/history/i, History],
		[/artist/i, Users]
	];

	interface Props {
		prompt: ServicePrompt;
		ondismiss?: ((service: string) => void) | undefined;
		/** A short card for a row of three: title and Connect on one line. */
		compact?: boolean;
	}

	let { prompt, ondismiss = undefined, compact = false }: Props = $props();

	const brand = $derived(serviceBrands[prompt.service] ?? otherBrand);

	function featureIcon(feature: string): ComponentType {
		return featureIcons.find(([pattern]) => pattern.test(feature))?.[1] ?? Check;
	}

	function getSettingsLink(service: string): string {
		// Per-user scrobble/discovery connections live on /profile, not the admin /settings tabs.
		if (service === 'listenbrainz' || service === 'lastfm')
			return withBasePath('/profile#scrobbling');
		return withBasePath(`/settings?tab=${service}`);
	}

	function handleDismiss() {
		dismiss(prompt.service);
		ondismiss?.(prompt.service);
	}

	const ServiceIcon = $derived(serviceIcons[prompt.service] || Music);
</script>

{#if compact}
	<Card variant="bordered" padding="none" class="h-full overflow-hidden border-l-4 {brand.edge}">
		<div class="flex h-full flex-col gap-2.5 p-4">
			<div class="flex items-center gap-3">
				<ServiceIcon class="size-8 shrink-0 {brand.mark}" aria-hidden="true" />
				<h3 class="min-w-0 flex-1 font-display text-xl leading-none uppercase">{prompt.title}</h3>
				<Button variant="primary" size="xs" icon={Link} href={getSettingsLink(prompt.service)}>
					Connect
				</Button>
				<IconButton size="xs" icon={X} label="Dismiss {prompt.title}" onclick={handleDismiss} />
			</div>
			<p class="text-xs leading-relaxed text-muted">{prompt.description}</p>
			<ul class="mt-auto hidden flex-wrap gap-x-4 gap-y-1 pt-0.5 sm:flex">
				{#each prompt.features as feature, i (`${feature}-${i}`)}
					{@const FeatureIcon = featureIcon(feature)}
					<li class="flex items-center gap-1.5 text-[0.6875rem] font-semibold">
						<FeatureIcon class="size-3 text-title" aria-hidden="true" />
						{feature}
					</li>
				{/each}
			</ul>
		</div>
	</Card>
{:else}
	<Card variant="bordered" padding="none" class="overflow-hidden border-l-4 {brand.edge}">
		<IconButton
			size="xs"
			icon={X}
			label="Dismiss {prompt.title}"
			class="absolute top-2 right-2"
			onclick={handleDismiss}
		/>
		<div class="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:gap-5 sm:p-5">
			<div class="flex min-w-0 flex-1 gap-4">
				<div class="shrink-0 pt-0.5 {brand.mark}">
					<ServiceIcon class="size-9 sm:size-11" aria-hidden="true" />
				</div>
				<div class="min-w-0 flex-1 pr-6">
					<h3 class="font-display text-xl leading-none sm:text-2xl">{prompt.title}</h3>
					<p class="mt-1.5 text-sm text-muted">{prompt.description}</p>
					<ul class="mt-2.5 hidden flex-wrap gap-x-5 gap-y-1.5 sm:flex">
						{#each prompt.features as feature, i (`${feature}-${i}`)}
							{@const FeatureIcon = featureIcon(feature)}
							<li class="flex items-center gap-1.5 text-xs font-semibold">
								<FeatureIcon class="size-3.5 text-title" aria-hidden="true" />
								{feature}
							</li>
						{/each}
					</ul>
				</div>
			</div>
			<div class="shrink-0 self-end sm:self-center sm:pt-4">
				<Button variant="primary" size="sm" icon={Link} href={getSettingsLink(prompt.service)}>
					Connect
				</Button>
			</div>
		</div>
	</Card>
{/if}
