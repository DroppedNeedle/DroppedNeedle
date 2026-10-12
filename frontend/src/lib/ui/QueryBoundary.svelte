<!--
	Renders one TanStack Query result: a loader while it is pending, an error
	with a retry when it failed, an empty state when it came back empty, and
	the data otherwise. Data already on screen stays there when a background
	refetch fails.
	Replaces the per-component `{#if query.isLoading}` / `isError` ladders and
	closes the gap in the components that never check for an error.
-->
<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import ServiceErrorState from '$lib/components/ServiceErrorState.svelte';

	interface QueryLike {
		data: T | undefined;
		isPending: boolean;
		isError: boolean;
		error: unknown;
		refetch: () => unknown;
	}

	interface Props {
		query: QueryLike;
		/** What is loading, for the error sentence: "Home", "your requests". */
		what: string;
		/** Renders the data. */
		children: Snippet<[T]>;
		/** The matching skeleton. A spinner when left out. */
		loading?: Snippet;
		/** Shown in place of the data when it is empty. Left out, empty data goes to `children`. */
		empty?: Snippet;
		/** Replaces the stock error state. Gets the error and a retry function. */
		failed?: Snippet<[unknown, () => void]>;
		/** Defaults to: null, an empty array, or an object whose `items` is an empty array. */
		isEmpty?: (data: T) => boolean;
	}

	let { query, what, children, loading, empty, failed, isEmpty = looksEmpty }: Props = $props();

	function looksEmpty(data: T): boolean {
		if (data === null) return true;
		if (Array.isArray(data)) return data.length === 0;
		if (typeof data === 'object' && 'items' in data && Array.isArray(data.items)) {
			return data.items.length === 0;
		}
		return false;
	}

	function retry() {
		void query.refetch();
	}
</script>

{#if query.data !== undefined}
	{#if empty && isEmpty(query.data)}
		{@render empty()}
	{:else}
		{@render children(query.data)}
	{/if}
{:else if query.isError}
	{#if failed}
		{@render failed(query.error, retry)}
	{:else}
		<ServiceErrorState error={query.error} {what} onretry={retry} />
	{/if}
{:else if query.isPending}
	{#if loading}
		{@render loading()}
	{:else}
		<div class="flex justify-center py-12" role="status" aria-label="Loading {what}">
			<span class="loading loading-spinner loading-lg" aria-hidden="true"></span>
		</div>
	{/if}
{/if}
