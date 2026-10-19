<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { api } from '$lib/api/client';
	import type { EventsSettings } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { CalendarClock, ExternalLink } from 'lucide-svelte';
	import { onDestroy, onMount } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

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

<Card variant="bordered">
	{#snippet header()}
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary/10 text-primary ring-1 ring-primary/20"
			>
				<CalendarClock class="h-5 w-5" aria-hidden="true" />
			</div>
			<div>
				<CardTitle>Live Events</CardTitle>
				<p class="text-sm text-base-content/60">
					Concert listings for followed artists, shown on the Following page.
				</p>
			</div>
		</div>
	{/snippet}
	<div class="mt-2 flex flex-col gap-4">
		{#if form.loading}
			<div class="flex justify-center py-10">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<ToggleRow label="Enable live events" bind:checked={form.data.enabled} />

			<Card variant="bordered" padding="md">
				<fieldset>
					<legend class="mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50">
						Search for concerts by
					</legend>
					<label class="flex cursor-pointer items-center gap-3 py-1">
						<input
							type="radio"
							class="radio radio-sm"
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
							class="radio radio-sm"
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
			</Card>

			<Card variant="bordered" padding="md">
				<div class="space-y-3">
					<div class="flex items-center justify-between">
						<div>
							<p class="font-medium">Ticketmaster</p>
							<p class="text-xs text-base-content/60">
								Worldwide coverage (USA + most of Europe). Free key, 5,000 calls/day.
							</p>
						</div>
						<input
							type="checkbox"
							class="toggle toggle-sm toggle-primary"
							bind:checked={form.data.ticketmaster_enabled}
							aria-label="Enable Ticketmaster"
						/>
					</div>
					<SecretField
						label="Consumer key"
						mask={SECRET_MASK}
						placeholder="Consumer Key from developer.ticketmaster.com"
						bind:value={form.data.ticketmaster_api_key}
					/>
					<div class="flex flex-wrap items-center gap-3">
						<Button
							variant="outline"
							size="sm"
							pill
							loading={ticketmasterTest.testing}
							onclick={() => runTest('ticketmaster', ticketmasterTest)}
						>
							Test
						</Button>
						{#if ticketmasterTest.message}
							<p
								class="text-sm"
								class:text-success={ticketmasterTest.valid}
								class:text-error={!ticketmasterTest.valid}
							>
								{ticketmasterTest.message}
							</p>
						{/if}
					</div>
					<a
						href="https://developer.ticketmaster.com/"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" aria-hidden="true" /> Get a free Ticketmaster key
					</a>
				</div>
			</Card>

			<Card variant="bordered" padding="md">
				<div class="space-y-3">
					<div class="flex items-center justify-between">
						<div>
							<p class="font-medium">Skiddle</p>
							<p class="text-xs text-base-content/60">
								Covers the UK & Ireland: small venues, club nights, festivals.
							</p>
						</div>
						<input
							type="checkbox"
							class="toggle toggle-sm toggle-primary"
							bind:checked={form.data.skiddle_enabled}
							aria-label="Enable Skiddle"
						/>
					</div>
					<SecretField
						label="API key"
						mask={SECRET_MASK}
						placeholder="API key from skiddle.com/api"
						bind:value={form.data.skiddle_api_key}
					/>
					<div class="flex flex-wrap items-center gap-3">
						<Button
							variant="outline"
							size="sm"
							pill
							loading={skiddleTest.testing}
							onclick={() => runTest('skiddle', skiddleTest)}
						>
							Test
						</Button>
						{#if skiddleTest.message}
							<p
								class="text-sm"
								class:text-success={skiddleTest.valid}
								class:text-error={!skiddleTest.valid}
							>
								{skiddleTest.message}
							</p>
						{/if}
					</div>
					<a
						href="https://www.skiddle.com/api/join.php"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" aria-hidden="true" /> Get a free Skiddle key
					</a>
				</div>
			</Card>

			<Field label="Check for new events daily at">
				{#snippet children(control)}
					<div class="flex items-center gap-2">
						<Input
							{...control}
							label="Check for new events daily at"
							type="time"
							class="w-32"
							bind:value={form.data!.poll_time}
						/>
						<span class="text-sm text-base-content/60">server time</span>
					</div>
				{/snippet}
			</Field>

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
				<Button variant="primary" pill loading={form.saving} onclick={() => void form.save()}>
					Save settings
				</Button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</Card>
