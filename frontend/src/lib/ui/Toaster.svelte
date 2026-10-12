<!--
	Shows the current toast from `toastStore`, the store nearly every screen
	already calls. Mount it once in the app shell.
	Replaces the shell's inline toast markup, the `Toast` component with its
	page-local state, the inline `class="toast"` blocks on the search pages and
	the separate playback toast.
-->
<script lang="ts">
	import { Check, Info, TriangleAlert, X } from 'lucide-svelte';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		/** Lifts the toast clear of the player bar. */
		abovePlayer?: boolean;
	}

	let { abovePlayer = false }: Props = $props();

	const tones: Record<string, string> = {
		success: 'alert-success',
		error: 'alert-error',
		warning: 'alert-warning',
		info: 'alert-info'
	};
</script>

{#if $toastStore}
	{@const tone: string = $toastStore.type}
	<div
		class="droppedneedle-playback-toast fixed left-1/2 z-50 -translate-x-1/2"
		class:droppedneedle-playback-toast--player={abovePlayer}
	>
		<div
			role={tone === 'error' ? 'alert' : 'status'}
			class="alert {tones[tone] ??
				tones.info} min-w-64 max-w-md px-4 py-2 shadow-lg motion-safe:animate-fade-in-up"
		>
			{#if tone === 'success'}
				<Check class="size-5 shrink-0" aria-hidden="true" />
			{:else if tone === 'error'}
				<X class="size-5 shrink-0" aria-hidden="true" />
			{:else if tone === 'warning'}
				<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
			{:else}
				<Info class="size-5 shrink-0" aria-hidden="true" />
			{/if}
			<span class="text-sm">{$toastStore.message}</span>
			<button
				type="button"
				class="btn btn-circle btn-ghost btn-xs"
				aria-label="Dismiss"
				onclick={() => toastStore.hide()}
			>
				<X class="size-3.5" aria-hidden="true" />
			</button>
		</div>
	</div>
{/if}
