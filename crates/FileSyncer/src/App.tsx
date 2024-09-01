import { useState, useContext } from "react";
import "./App.css";
import { Box, Button, Paper, TextareaAutosize, Typography } from "@mui/material";
import { TaskDispatcherContext } from "./context/TaskDispatcher";

function App() {
  const dispatcher = useContext(TaskDispatcherContext);

  const [mounted, setMounted] = useState(false);
  const [ready, setReady] = useState(false);
  const [messages, setMessages] = useState<string[]>([]);

  if (!mounted) {
    setMounted(true);
    dispatcher.listen().then(() => setReady(true));
  }

  return (
    <TaskDispatcherContext.Provider value={dispatcher}>
      <Paper>
        {
          ready ? (
            <Box sx={{ p: 2 }}>
              <Typography variant="h3">Actions</Typography>
              <Box sx={{ display: "flex", gap: 2 }}>
                <Button onClick={() => dispatcher.submit("InitSpawn")
                  .then((result) => {
                    setMessages([...messages, JSON.stringify(result)]);
                  })
                } variant="contained">InitSpawn</Button>
                <Button onClick={() => dispatcher.submit("ListMounts")
                  .then((result) => {
                    setMessages([...messages, JSON.stringify(result)]);
                  })
                } variant="contained">ListMounts</Button>
              </Box>
              <Typography variant="h3">Messages</Typography>
              <TextareaAutosize value={messages.join("\n")} readOnly />
            </Box>
          ) : (
            <Typography variant="h3">Loading...</Typography>
          )
        }
      </Paper >
    </TaskDispatcherContext.Provider>
  );
}

export default App;
