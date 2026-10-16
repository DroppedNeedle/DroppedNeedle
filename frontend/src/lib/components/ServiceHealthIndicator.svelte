<script module lang="ts">
	import { SvelteMap } from 'svelte/reactivity';

	const NOTIFICATION_COOLDOWN_MS = 10 * 60 * 1000;
	const notifiedAt = new SvelteMap<string, number>();
</script>

<script lang="ts">
	import { Activity, TriangleAlert } from 'lucide-svelte';
	import { getSystemHealthQuery } from '$lib/queries/system/SystemHealthQuery.svelte';
	import { toastStore } from '$lib/stores/toast';
	import IconButton from '$lib/ui/IconButton.svelte';
	import type { ServiceHealthItem } from '$lib/types';

	const query = getSystemHealthQuery();
	const degraded = $derived<ServiceHealthItem[]>(query.data?.degraded ?? []);
	const hasDegradation = $derived(degraded.length > 0);

	function serviceLabel(s: string): string {
		if (s === 'listenbrainz') return 'ListenBrainz';
		if (s === 'lastfm') return 'Last.fm';
		if (s === 'musicbrainz') return 'MusicBrainz';
		if (s === 'audiodb') return 'TheAudioDB';
		if (s === 'wikidata') return 'Wikipedia';
		if (s === 'acquisition_cleanup') return 'Download cleanup';
		return s.charAt(0).toUpperCase() + s.slice(1);
	}

	function fallbackLabel(f: string | null | undefined): string | null {
		if (!f) return null;
		return serviceLabel(f);
	}

	// Track each service:capability independently so a healing outage stays quiet
	// when it returns, while a newly degraded capability can still speak up.
	$effect(() => {
		const now = Date.now();
		for (const [key, lastNotified] of notifiedAt) {
			if (now - lastNotified >= NOTIFICATION_COOLDOWN_MS) {
				notifiedAt.delete(key);
			}
		}

		const eligible = degraded.filter((item) => {
			const key = `${item.service}:${item.capability}`;
			return !notifiedAt.has(key);
		});

		if (!eligible.length) return;

		for (const item of eligible) {
			notifiedAt.set(`${item.service}:${item.capability}`, now);
		}

		const names = [...new Set(eligible.map((d) => serviceLabel(d.service)))];
		const cleanup = eligible.find((d) => d.service === 'acquisition_cleanup');
		if (cleanup && eligible.length === 1) {
			// the backend message already carries the retry note - show it verbatim
			toastStore.show({
				message: cleanup.message,
				type: 'info'
			});
			return;
		}
		if (cleanup) {
			const serviceVerb = names.length === 1 ? 'is' : 'are';
			toastStore.show({
				message: `${names.join(', ')} ${serviceVerb} having problems. Retrying automatically.`,
				type: 'info'
			});
			return;
		}
		// only claim "using a fallback" when every eligible service has one; most
		// (MusicBrainz, Last.fm, Wikipedia) don't, and the blanket line would lie
		const tail = eligible.every((d) => d.fallback) ? 'using a fallback for now.' : 'auto-retrying.';
		toastStore.show({
			message: `Trouble reaching ${names.join(', ')} - ${tail}`,
			type: 'info'
		});
	});

	let open = $state(false);
	function toggle() {
		open = !open;
	}
	function close() {
		open = false;
	}
</script>

{#if hasDegradation}
	<div class="relative">
		<IconButton
			icon={Activity}
			label="Service status: {degraded.length} degraded"
			aria-expanded={open}
			class="text-warning"
			onclick={toggle}
		/>
		<span
			class="pointer-events-none absolute right-1.5 top-1.5 h-2.5 w-2.5 rounded-full bg-warning ring-2 ring-base-100"
		></span>

		{#if open}
			<!-- click-away scrim -->
			<button class="fixed inset-0 z-40 cursor-default" aria-label="Close status" onclick={close}
			></button>
			<div
				class="absolute right-0 z-50 mt-1 w-72 overflow-hidden rounded-xl border border-base-300 bg-base-100 shadow-xl"
				role="dialog"
				aria-label="Service status"
			>
				<div class="flex items-center gap-2 border-b border-base-200 px-4 py-2.5">
					<TriangleAlert class="h-4 w-4 text-warning" />
					<span class="text-sm font-semibold">
						{degraded.length}
						{degraded.length === 1 ? 'service' : 'services'} degraded
					</span>
				</div>
				<ul class="divide-y divide-base-200">
					{#each degraded as item (item.service + ':' + item.capability)}
						{@const fb = fallbackLabel(item.fallback)}
						<li class="px-4 py-3">
							<div class="flex items-center gap-2">
								<span class="h-2 w-2 shrink-0 rounded-full bg-warning"></span>
								<span class="text-sm font-medium">{serviceLabel(item.service)}</span>
								<span class="text-xs text-base-content/40">· {item.capability}</span>
							</div>
							<p class="mt-1 pl-4 text-xs text-base-content/60">{item.message}</p>
							{#if fb}
								<p class="mt-0.5 pl-4 text-xs text-base-content/45">
									Using <span class="font-medium text-base-content/70">{fb}</span> instead. Auto-retrying.
								</p>
							{/if}
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	</div>
{/if}
