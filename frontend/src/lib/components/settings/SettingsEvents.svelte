<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { api } from '$lib/api/client';
	import type { EventsSettings } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { CalendarClock, ExternalLink } from 'lucide-svelte';
	import { onDestroy, onMount } from 'svelte';

	const form = createSettingsForm<EventsSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.events(),
		saveEndpoint: SETTINGS_ENDPOINTS.events(),
		afterSave: async () => {
			// the concerts response carries the `configured` flag and the save
			// kicks a backend sweep - refetch so the events page reflects both
			const userId = authStore.user?.id;
			await invalidateQueriesWithPersister({
				queryKey: FollowQueryKeyFactory.concerts(userId)
			});
			await invalidateQueriesWithPersister({
				queryKey: FollowQueryKeyFactory.concertsUnseen(userId)
			});
		}
	});

	let showTicketmasterKey = $state(false);
	let showSkiddleKey = $state(false);

	// two sources = two independent test buttons (the shared form util only
	// supports one testEndpoint)
	type TestState = { testing: boolean; message: string; valid: boolean };
	let ticketmasterTest = $state<TestState>({ testing: false, message: '', valid: false });
	let skiddleTest = $state<TestState>({ testing: false, message: '', valid: false });

	async function runTest(provider: 'ticketmaster' | 'skiddle', state: TestState) {
		if (!form.data) return;
		state.testing = true;
		state.message = '';
		try {
			const endpoint =
				provider === 'ticketmaster'
					? SETTINGS_ENDPOINTS.eventsTestTicketmaster()
					: SETTINGS_ENDPOINTS.eventsTestSkiddle();
			const result = await api.global.v3.POST(endpoint, form.data);
			state.valid = result.valid;
			state.message = result.message;
		} catch {
			state.valid = false;
			state.message = "The test couldn't run. Check your connection and try again.";
		} finally {
			state.testing = false;
		}
	}

	onMount(() => form.load());
	onDestroy(() => form.cleanup());
</script>

<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
	<div class="card-body gap-4">
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary/10 text-primary ring-1 ring-primary/20"
			>
				<CalendarClock class="h-5 w-5" aria-hidden="true" />
			</div>
			<div>
				<h2 class="card-title text-2xl">Live Events</h2>
				<p class="text-sm text-base-content/60">
					Concert listings for followed artists, shown on the Following page.
				</p>
			</div>
		</div>

		{#if form.loading}
			<div class="flex justify-center py-10">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<label class="flex cursor-pointer items-center gap-3">
				<input type="checkbox" class="toggle toggle-primary" bind:checked={form.data.enabled} />
				<span class="text-sm">Enable live events</span>
			</label>

			<fieldset class="rounded-xl border border-base-300/50 bg-base-300/20 p-4">
				<legend class="px-1 text-xs font-semibold uppercase tracking-wider text-base-content/50">
					Search for concerts by
				</legend>
				<label class="flex cursor-pointer items-center gap-3 py-1">
					<input
						type="radio"
						class="radio radio-primary radio-sm"
						value="followed"
						bind:group={form.data.sweep_scope}
					/>
					<span class="text-sm">
						Followed artists only
						<span class="block text-xs text-base-content/60">
							Checks the artists people follow. Light on API usage.
						</span>
					</span>
				</label>
				<label class="flex cursor-pointer items-center gap-3 py-1">
					<input
						type="radio"
						class="radio radio-primary radio-sm"
						value="library"
						bind:group={form.data.sweep_scope}
					/>
					<span class="text-sm">
						Every artist in the library
						<span class="block text-xs text-base-content/60">
							Also checks the whole library and shows those gigs to everyone. Very large libraries
							are spread across several days to respect API limits.
						</span>
					</span>
				</label>
			</fieldset>

			<!-- Ticketmaster -->
			<div class="rounded-xl border border-base-300/50 bg-base-300/20 p-4">
				<div class="mb-3 flex items-center justify-between">
					<div>
						<p class="font-medium">Ticketmaster</p>
						<p class="text-xs text-base-content/60">
							Worldwide coverage (USA + most of Europe). Free key, 5,000 calls/day.
						</p>
					</div>
					<input
						type="checkbox"
						class="toggle toggle-primary"
						bind:checked={form.data.ticketmaster_enabled}
						aria-label="Enable Ticketmaster"
					/>
				</div>
				<div class="flex items-center gap-2">
					<div class="relative flex-1">
						<input
							type={showTicketmasterKey ? 'text' : 'password'}
							class="input input-soft w-full pr-16"
							bind:value={form.data.ticketmaster_api_key}
							placeholder="Consumer Key from developer.ticketmaster.com"
							autocomplete="off"
						/>
						<button
							type="button"
							class="btn btn-ghost btn-xs absolute right-2 top-1/2 -translate-y-1/2 rounded-full"
							onclick={() => (showTicketmasterKey = !showTicketmasterKey)}
						>
							{showTicketmasterKey ? 'Hide' : 'Show'}
						</button>
					</div>
					<button
						type="button"
						class="btn btn-outline btn-sm rounded-full"
						onclick={() => runTest('ticketmaster', ticketmasterTest)}
						disabled={ticketmasterTest.testing}
					>
						{#if ticketmasterTest.testing}
							<span class="loading loading-spinner loading-xs"></span>
						{/if}
						Test
					</button>
				</div>
				{#if ticketmasterTest.message}
					<p
						class="mt-2 text-sm"
						class:text-success={ticketmasterTest.valid}
						class:text-error={!ticketmasterTest.valid}
					>
						{ticketmasterTest.message}
					</p>
				{/if}
				<a
					href="https://developer.ticketmaster.com/"
					target="_blank"
					rel="noopener noreferrer"
					class="mt-2 flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
				>
					<ExternalLink class="h-3 w-3" aria-hidden="true" /> Get a free Ticketmaster key
				</a>
			</div>

			<!-- Skiddle -->
			<div class="rounded-xl border border-base-300/50 bg-base-300/20 p-4">
				<div class="mb-3 flex items-center justify-between">
					<div>
						<p class="font-medium">Skiddle</p>
						<p class="text-xs text-base-content/60">
							Covers the UK & Ireland: small venues, club nights, festivals.
						</p>
					</div>
					<input
						type="checkbox"
						class="toggle toggle-primary"
						bind:checked={form.data.skiddle_enabled}
						aria-label="Enable Skiddle"
					/>
				</div>
				<div class="flex items-center gap-2">
					<div class="relative flex-1">
						<input
							type={showSkiddleKey ? 'text' : 'password'}
							class="input input-soft w-full pr-16"
							bind:value={form.data.skiddle_api_key}
							placeholder="API key from skiddle.com/api"
							autocomplete="off"
						/>
						<button
							type="button"
							class="btn btn-ghost btn-xs absolute right-2 top-1/2 -translate-y-1/2 rounded-full"
							onclick={() => (showSkiddleKey = !showSkiddleKey)}
						>
							{showSkiddleKey ? 'Hide' : 'Show'}
						</button>
					</div>
					<button
						type="button"
						class="btn btn-outline btn-sm rounded-full"
						onclick={() => runTest('skiddle', skiddleTest)}
						disabled={skiddleTest.testing}
					>
						{#if skiddleTest.testing}
							<span class="loading loading-spinner loading-xs"></span>
						{/if}
						Test
					</button>
				</div>
				{#if skiddleTest.message}
					<p
						class="mt-2 text-sm"
						class:text-success={skiddleTest.valid}
						class:text-error={!skiddleTest.valid}
					>
						{skiddleTest.message}
					</p>
				{/if}
				<a
					href="https://www.skiddle.com/api/join.php"
					target="_blank"
					rel="noopener noreferrer"
					class="mt-2 flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
				>
					<ExternalLink class="h-3 w-3" aria-hidden="true" /> Get a free Skiddle key
				</a>
			</div>

			<label class="form-control w-fit">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					Check for new events daily at
				</span>
				<div class="flex items-center gap-2">
					<input type="time" class="input input-soft w-32" bind:value={form.data.poll_time} />
					<span class="text-sm text-base-content/60">server time</span>
				</div>
			</label>

			{#if form.message}
				<div
					class="alert"
					class:alert-success={form.messageType === 'success'}
					class:alert-error={form.messageType === 'error'}
				>
					<span>{form.message}</span>
				</div>
			{/if}

			<div class="flex justify-end pt-1">
				<button
					type="button"
					class="btn btn-primary glow-primary-soft gap-2 rounded-full"
					onclick={() => void form.save()}
					disabled={form.saving}
				>
					{#if form.saving}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Save settings
				</button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</div>
