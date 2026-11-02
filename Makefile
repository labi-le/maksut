.PHONY: help logs logs-bot logs-cookies logs-deploy logs-improve deploy-status deploy-now

TAIL ?= 100

help:
	@printf "Targets:\n"
	@printf "  make logs            Follow all logs (maksut + cookie-refresh + auto-deploy + self-improve)\n"
	@printf "  make logs-bot        Follow maksut container logs (TAIL=$(TAIL))\n"
	@printf "  make logs-cookies    Follow cookie-refresh container logs (TAIL=$(TAIL))\n"
	@printf "  make logs-deploy     Follow maksut-auto-deploy.service journal logs (TAIL=$(TAIL))\n"
	@printf "  make logs-improve    Follow maksut-self-improve.service journal logs (TAIL=$(TAIL))\n"
	@printf "  make deploy-status   Show deployed SHA, HEAD, origin/main, container image, and recent auto-deploy events\n"
	@printf "  make deploy-now      Trigger maksut-auto-deploy.service immediately and show status\n"

logs:
	@trap 'trap - INT TERM EXIT; kill 0 2>/dev/null || true' INT TERM EXIT; \
	docker compose -f compose.yaml logs -f --tail $(TAIL) maksut cookie-refresh & \
	journalctl --user -u maksut-auto-deploy.service -u maksut-self-improve.service -f -n $(TAIL) --no-pager & \
	wait

logs-bot:
	docker compose -f compose.yaml logs -f --tail $(TAIL) maksut

logs-cookies:
	docker compose -f compose.yaml logs -f --tail $(TAIL) cookie-refresh

logs-deploy:
	journalctl --user -u maksut-auto-deploy.service -f -n $(TAIL) --no-pager

logs-improve:
	journalctl --user -u maksut-self-improve.service -f -n $(TAIL) --no-pager

deploy-status:
	@printf "HEAD:          %s\n" "$$(git rev-parse HEAD)"
	@git fetch --quiet origin main 2>/dev/null || true
	@printf "origin/main:   %s\n" "$$(git rev-parse origin/main 2>/dev/null || echo unknown)"
	@printf "deployed_sha:  %s\n" "$$(cat self-improve/deployed_sha 2>/dev/null || echo none)"
	@printf "container:     %s\n" "$$(docker inspect maksut-maksut-1 --format '{{.Config.Image}} (started {{.State.StartedAt}})' 2>/dev/null || echo not-running)"
	@printf "\nRecent auto-deploy actions:\n"
	@journalctl --user -u maksut-auto-deploy.service --grep="auto-deploy: (deployed|fast-forwarded|docker |git |unable )" -n 10 --no-pager || true
	@printf "\nLatest auto-deploy tick:\n"
	@journalctl --user -u maksut-auto-deploy.service --grep="auto-deploy:" -n 3 --no-pager || true

deploy-now:
	systemctl --user start maksut-auto-deploy.service
	@$(MAKE) --no-print-directory deploy-status
