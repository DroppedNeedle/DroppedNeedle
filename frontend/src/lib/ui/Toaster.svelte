<!--
	Shows the current toast from `toastStore`. Mount it once in the app shell.
	A solid surface with an edge and icon in the toast's tone; the words carry
	the meaning, never the colour alone. A toast may lead with bold words and
	carry up to four buttons: its action, then a question and its choices,
	which wrap to a second row when they do not fit.
-->
<script lang="ts">
	import { Check, Info, TriangleAlert, X } from 'lucide-svelte';
	import { toastStore, type ToastAction } from '$lib/stores/toast';
	import Button from './Button.svelte';
	import IconButton from './IconButton.svelte';

	interface Props {
		/** Lifts the toast clear of the player bar. */
		abovePlayer?: boolean;
	}

	let { abovePlayer = false }: Props = $props();

	// Whole class strings, so Tailwind's scan finds every one.
	const tones: Record<string, { edge: string; icon: string }> = {
		success: { edge: 'border-l-success', icon: 'text-success' },
		error: { edge: 'border-l-error', icon: 'text-error' },
		warning: { edge: 'border-l-warning', icon: 'text-warning' },
		info: { edge: 'border-l-info', icon: 'text-info' }
	};

	function runAction(action: ToastAction) {
		// Taken before hiding: hiding empties the toast the action reads from.
		const { run } = action;
		toastStore.hide();
		void run();
	}
</script>

{#if $toastStore}
	{@const tone: string = $toastStore.type}
	{@const look = tones[tone] ?? tones.info}
	{@const choices = $toastStore.choices}
	<div
		class="droppedneedle-toast fixed left-1/2 z-50 -translate-x-1/2"
		class:droppedneedle-toast--player={abovePlayer}
	>
		<div
			role={tone === 'error' ? 'alert' : 'status'}
			class="flex min-w-64 max-w-md items-center gap-3 rounded-box border border-l-4 border-line bg-base-200 py-2 pr-2 pl-3 text-base-content shadow-lg motion-safe:animate-fade-in-up {look.edge}"
		>
			{#if tone === 'success'}
				<Check class="size-5 shrink-0 {look.icon}" aria-hidden="true" />
			{:else if tone === 'error'}
				<X class="size-5 shrink-0 {look.icon}" aria-hidden="true" />
			{:else if tone === 'warning'}
				<TriangleAlert class="size-5 shrink-0 {look.icon}" aria-hidden="true" />
			{:else}
				<Info class="size-5 shrink-0 {look.icon}" aria-hidden="true" />
			{/if}
			<div class="flex grow flex-wrap items-center gap-x-3 gap-y-2">
				<span class="text-sm">
					{#if $toastStore.title}<strong>{$toastStore.title}</strong>{/if}
					{$toastStore.message}
				</span>
				{#if $toastStore.action || choices}
					<div class="flex flex-wrap items-center gap-2">
						{#if $toastStore.action}
							{@const action = $toastStore.action}
							<Button variant="outline" size="xs" onclick={() => runAction(action)}
								>{action.label}</Button
							>
						{/if}
						{#if choices}
							<span class="font-mono text-xs tracking-wide text-muted uppercase">
								{choices.ask}
							</span>
							{#each choices.actions.slice(0, 3) as choice (choice.label)}
								<Button variant="outline" size="xs" onclick={() => runAction(choice)}
									>{choice.label}</Button
								>
							{/each}
						{/if}
					</div>
				{/if}
			</div>
			<IconButton size="xs" icon={X} label="Dismiss" onclick={() => toastStore.hide()} />
		</div>
	</div>
{/if}
