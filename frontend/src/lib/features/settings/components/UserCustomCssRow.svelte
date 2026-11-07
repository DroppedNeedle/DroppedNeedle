<!--
	One person's custom CSS as an admin sees it: on or off, its length and
	when it was saved, never the text. Turn off clears their "Use my CSS"
	switch and keeps the text, so they can switch it back on themselves.
-->
<script lang="ts">
	import {
		createTurnOffUserCustomCssMutation,
		getUserCustomCssQuery,
		type CustomCssSummary
	} from '$lib/features/settings/queries/appearance.svelte';
	import { ApiError } from '$lib/api/errors';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { formatRelativeTime } from '$lib/utils/formatting';

	interface Props {
		userId: string;
		displayName: string;
	}

	let { userId, displayName }: Props = $props();

	const summary = getUserCustomCssQuery(() => userId);
	const turnOff = createTurnOffUserCustomCssMutation();
	let failure = $state<string | null>(null);

	function described(css: CustomCssSummary): string {
		if (!css.has_css) return 'None';
		const parts = [css.enabled ? 'On' : 'Off', `${css.length.toLocaleString()} characters`];
		if (css.saved_at) parts.push(`saved ${formatRelativeTime(new Date(css.saved_at))}`);
		return parts.join(' · ');
	}

	async function off(): Promise<void> {
		failure = null;
		try {
			await turnOff.mutateAsync(userId);
		} catch (error) {
			failure =
				error instanceof ApiError && error.status < 500
					? error.message
					: 'Could not turn it off. Try again.';
		}
	}
</script>

<div class="flex items-center justify-between gap-3 px-1 py-2">
	<div class="min-w-0">
		<p class="text-sm font-semibold">Custom CSS</p>
		<QueryBoundary query={summary} what="their custom CSS">
			{#snippet loading()}
				<span class="skeleton block h-4 w-40" aria-hidden="true"></span>
			{/snippet}
			{#snippet children(css)}
				<p class="text-sm text-muted">{described(css)}</p>
				{#if css.has_css && !css.enabled}
					<p class="text-xs text-muted">They can turn it back on in Profile.</p>
				{/if}
			{/snippet}
		</QueryBoundary>
		{#if failure}
			<p class="text-xs text-error" role="alert">{failure}</p>
		{/if}
	</div>
	{#if summary.data?.has_css && summary.data.enabled}
		<AsyncButton
			variant="danger-outline"
			size="sm"
			class="shrink-0"
			onclick={off}
			aria-label={`Turn off ${displayName}'s custom CSS`}
		>
			Turn off
		</AsyncButton>
	{/if}
</div>
