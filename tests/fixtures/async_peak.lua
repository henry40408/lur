-- Launch several async tasks that each overlap at an await point, tracking the
-- peak number simultaneously in flight. With --max-concurrency N the peak must
-- not exceed N; uncapped it equals the task count. Prints the observed peak.
local inflight = 0
local peak = 0

local function task()
	inflight = inflight + 1
	if inflight > peak then
		peak = inflight
	end
	lur.async.sleep(20)
	inflight = inflight - 1
end

lur.async.all({ task, task, task, task, task, task })
lur.stdout.write(tostring(peak))
